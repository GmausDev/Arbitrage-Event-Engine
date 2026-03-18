// agents/news_agent/src/lib.rs
// Ingests news/RSS feeds, scores sentiment, and emits SentimentUpdate events
// so the bayesian_engine can adjust its probability priors.
//
// Subscribes to: nothing (source agent)
// Publishes:     Event::Sentiment(SentimentUpdate)

use chrono::Utc;
use common::{Event, EventBus, SentimentUpdate};
use std::time::Duration;
use tokio::time;
use tracing::{error, info, warn};

const POLL_INTERVAL_MS: u64 = 5_000;

/// A raw news item before scoring.
#[derive(Debug)]
struct RawNewsItem {
    pub headline: String,
    pub related_market_ids: Vec<String>,
}

pub struct NewsAgent {
    bus: EventBus,
    // TODO: add HTTP client for news feeds / websocket connection
}

impl NewsAgent {
    pub fn new(bus: EventBus) -> Self {
        Self { bus }
    }

    /// Fetch raw news items from configured feeds.
    ///
    /// TODO: support RSS feeds, Twitter/X streams, Manifold comments, etc.
    async fn fetch_news(&self) -> anyhow::Result<Vec<RawNewsItem>> {
        Ok(vec![])
    }

    /// Score the sentiment of a headline.
    ///
    /// TODO: integrate an LLM or simple keyword-based classifier.
    fn score_sentiment(&self, headline: &str) -> f64 {
        // Stub: neutral sentiment
        let _ = headline;
        0.0
    }

    pub async fn run(self) {
        let mut interval = time::interval(Duration::from_millis(POLL_INTERVAL_MS));
        info!("news_agent: started, polling every {POLL_INTERVAL_MS}ms");

        loop {
            interval.tick().await;

            let items = match self.fetch_news().await {
                Ok(i) => i,
                Err(e) => {
                    error!("news_agent: fetch error: {e}");
                    continue;
                }
            };

            for item in items {
                let score = self.score_sentiment(&item.headline);
                let update = SentimentUpdate {
                    related_market_ids: item.related_market_ids,
                    headline: item.headline,
                    sentiment_score: score,
                    timestamp: Utc::now(),
                };
                if let Err(e) = self.bus.publish(Event::Sentiment(update)) {
                    warn!("news_agent: failed to publish SentimentUpdate: {e}");
                }
            }
        }
    }
}
