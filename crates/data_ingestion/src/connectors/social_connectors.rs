// crates/data_ingestion/src/connectors/social_connectors.rs

use anyhow::Result;
use async_trait::async_trait;
use futures::future::join_all;
use serde::Deserialize;
use std::time::Duration;

use super::RawSocialPost;

#[async_trait]
pub trait SocialConnector: Send + Sync {
    fn name(&self) -> &str;
    /// Stream recent posts/mentions for tracked topics.
    async fn stream_posts(&self) -> Result<Vec<RawSocialPost>>;
    /// Detect trending topics above a mention-rate threshold.
    async fn detect_trends(&self, min_mention_rate: f64) -> Result<Vec<RawSocialPost>>;
}

// ---------------------------------------------------------------------------
// Reddit API serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RedditResponse {
    data: RedditListing,
}

#[derive(Deserialize)]
struct RedditListing {
    children: Vec<RedditChild>,
}

#[derive(Deserialize)]
struct RedditChild {
    data: RedditPost,
}

#[derive(Deserialize)]
struct RedditPost {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    subreddit: String,
    #[serde(default)]
    upvote_ratio: f64,
    #[serde(default)]
    num_comments: i64,
}

// ---------------------------------------------------------------------------
// Reddit connector
// ---------------------------------------------------------------------------

const REDDIT_SUBREDDITS: &[&str] = &["PredictionMarkets", "politics", "Economics", "investing"];

pub struct RedditConnector {
    client: reqwest::Client,
}

impl RedditConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            "prediction-market-bot/1.0".parse().unwrap(),
        );
        let client = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .timeout(Duration::from_millis(timeout_ms))
            .build()?;
        Ok(Self { client })
    }
}

impl Default for RedditConnector {
    fn default() -> Self {
        Self::new(10_000).unwrap_or_else(|e| {
            tracing::error!(err = %e, "RedditConnector: failed to build HTTP client, using no-op fallback");
            let headers = reqwest::header::HeaderMap::new();
            Self { client: reqwest::ClientBuilder::new().default_headers(headers).build().unwrap_or_default() }
        })
    }
}

#[async_trait]
impl SocialConnector for RedditConnector {
    fn name(&self) -> &str {
        "reddit"
    }

    async fn stream_posts(&self) -> Result<Vec<RawSocialPost>> {
        // Fetch all subreddits concurrently.
        let futs = REDDIT_SUBREDDITS.iter().map(|sub| {
            let url = format!("https://www.reddit.com/r/{sub}/hot.json?limit=25");
            let client = self.client.clone();
            async move {
                let resp = match client.get(&url).send().await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(connector = "reddit", subreddit = sub, err = %e, "fetch failed");
                        return None;
                    }
                };

                if !resp.status().is_success() {
                    tracing::warn!(
                        connector = "reddit",
                        subreddit = sub,
                        status = %resp.status(),
                        "HTTP error response"
                    );
                    return None;
                }

                let raw: RedditResponse = match resp.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(connector = "reddit", subreddit = sub, err = %e, "failed to parse response");
                        return None;
                    }
                };

                let posts = raw.data.children;
                if posts.is_empty() {
                    return None;
                }

                let mention_count = posts.len() as u64;
                let avg_upvote_ratio = posts.iter().map(|p| p.data.upvote_ratio).sum::<f64>()
                    / posts.len() as f64;
                let avg_comments = posts
                    .iter()
                    .map(|p| p.data.num_comments as f64)
                    .sum::<f64>()
                    / posts.len() as f64;

                let sentiment_score = avg_upvote_ratio * 2.0 - 1.0;
                let velocity = avg_comments / 25.0;

                Some(RawSocialPost {
                    topic: sub.to_string(),
                    mention_count,
                    rolling_window_secs: 3600,
                    sentiment_score,
                    velocity,
                })
            }
        });

        let fetched: Vec<Option<RawSocialPost>> = join_all(futs).await;
        let results: Vec<RawSocialPost> = fetched.into_iter().flatten().collect();

        tracing::info!(count = results.len(), "reddit: fetched {} subreddit aggregates", results.len());
        Ok(results)
    }

    async fn detect_trends(&self, min_mention_rate: f64) -> Result<Vec<RawSocialPost>> {
        let all = self.stream_posts().await?;
        Ok(all
            .into_iter()
            .filter(|p| {
                let rate = p.mention_count as f64 / p.rolling_window_secs as f64 * 60.0;
                rate >= min_mention_rate
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Twitter API serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TwitterResponse {
    #[serde(default)]
    data: Vec<Tweet>,
}

#[derive(Deserialize)]
struct Tweet {
    id: String,
    #[serde(default)]
    public_metrics: Option<TweetMetrics>,
}

#[derive(Deserialize, Default)]
struct TweetMetrics {
    #[serde(default)]
    like_count: u64,
    #[serde(default)]
    retweet_count: u64,
    #[serde(default)]
    reply_count: u64,
}

// ---------------------------------------------------------------------------
// Twitter connector
// ---------------------------------------------------------------------------

pub struct TwitterConnector {
    client: reqwest::Client,
}

impl TwitterConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client })
    }
}

impl Default for TwitterConnector {
    fn default() -> Self {
        Self::new(10_000).unwrap_or_else(|e| {
            tracing::error!(err = %e, "TwitterConnector: failed to build HTTP client, using no-op fallback");
            Self { client: reqwest::Client::new() }
        })
    }
}

#[async_trait]
impl SocialConnector for TwitterConnector {
    fn name(&self) -> &str {
        "twitter"
    }

    async fn stream_posts(&self) -> Result<Vec<RawSocialPost>> {
        let token = match std::env::var("TWITTER_BEARER_TOKEN") {
            Ok(t) => t,
            Err(_) => {
                tracing::warn!(connector = "twitter", "TWITTER_BEARER_TOKEN not set; skipping fetch");
                return Ok(vec![]);
            }
        };

        let url = "https://api.twitter.com/2/tweets/search/recent?query=prediction+market+economy+politics&max_results=100&tweet.fields=created_at,public_metrics";

        let resp = match self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(connector = "twitter", err = %e, "HTTP request failed");
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                connector = "twitter",
                status = %resp.status(),
                "HTTP error response"
            );
            return Ok(vec![]);
        }

        let raw: TwitterResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(connector = "twitter", err = %e, "failed to parse response");
                return Ok(vec![]);
            }
        };

        if raw.data.is_empty() {
            return Ok(vec![]);
        }

        let tweet_count = raw.data.len() as f64;
        let total_likes: u64 = raw
            .data
            .iter()
            .map(|t| t.public_metrics.as_ref().map(|m| m.like_count).unwrap_or(0))
            .sum();
        let velocity = total_likes as f64 / tweet_count;

        tracing::info!(count = raw.data.len(), "twitter: fetched {} tweets", raw.data.len());
        Ok(vec![RawSocialPost {
            topic: "twitter_prediction_market".into(),
            mention_count: raw.data.len() as u64,
            rolling_window_secs: 3600,
            sentiment_score: 0.0,
            velocity,
        }])
    }

    async fn detect_trends(&self, min_mention_rate: f64) -> Result<Vec<RawSocialPost>> {
        let all = self.stream_posts().await?;
        Ok(all
            .into_iter()
            .filter(|p| {
                let rate = p.mention_count as f64 / p.rolling_window_secs as f64 * 60.0;
                rate >= min_mention_rate
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Mock (tests)
// ---------------------------------------------------------------------------

pub struct MockSocialConnector {
    pub posts: Vec<RawSocialPost>,
}

impl MockSocialConnector {
    pub fn new(posts: Vec<RawSocialPost>) -> Self {
        Self { posts }
    }
}

#[async_trait]
impl SocialConnector for MockSocialConnector {
    fn name(&self) -> &str {
        "mock_social"
    }

    async fn stream_posts(&self) -> Result<Vec<RawSocialPost>> {
        Ok(self.posts.clone())
    }

    async fn detect_trends(&self, _min_mention_rate: f64) -> Result<Vec<RawSocialPost>> {
        Ok(self.posts.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_social_returns_all_posts() {
        let posts = vec![RawSocialPost {
            topic: "test_topic".into(),
            mention_count: 100,
            rolling_window_secs: 60,
            sentiment_score: 0.1,
            velocity: 0.5,
        }];
        let c = MockSocialConnector::new(posts);
        let result = c.stream_posts().await.unwrap();
        assert_eq!(result.len(), 1);
    }
}
