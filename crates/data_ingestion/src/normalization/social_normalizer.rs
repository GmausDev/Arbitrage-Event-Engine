// crates/data_ingestion/src/normalization/social_normalizer.rs

use chrono::Utc;
use common::events::SocialTrend;

use crate::connectors::RawSocialPost;

pub struct SocialNormalizer;

impl SocialNormalizer {
    /// Convert raw social posts into `SocialTrend` events.
    ///
    /// `mention_rate` is computed as mentions per minute over the rolling window.
    /// Posts with empty topic strings are discarded.
    pub fn normalize(raw: Vec<RawSocialPost>) -> Vec<SocialTrend> {
        raw.into_iter()
            .filter_map(|r| {
                if r.topic.is_empty() {
                    tracing::warn!("social_normalizer: skipping post with empty topic");
                    return None;
                }
                let window_mins = (r.rolling_window_secs as f64 / 60.0).max(1.0);
                let mention_rate = r.mention_count as f64 / window_mins;
                Some(SocialTrend {
                    topic: r.topic,
                    mention_rate,
                    sentiment: r.sentiment_score.clamp(-1.0, 1.0),
                    velocity: r.velocity,
                    timestamp: Utc::now(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(topic: &str, mentions: u64, window_secs: u64) -> RawSocialPost {
        RawSocialPost {
            topic: topic.into(),
            mention_count: mentions,
            rolling_window_secs: window_secs,
            sentiment_score: 0.1,
            velocity: 1.0,
        }
    }

    #[test]
    fn normalize_computes_mention_rate() {
        // 120 mentions over 3600 seconds (60 minutes) = 2 per minute
        let trends = SocialNormalizer::normalize(vec![raw("topic_a", 120, 3600)]);
        assert_eq!(trends.len(), 1);
        assert!((trends[0].mention_rate - 2.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_discards_empty_topic() {
        let trends = SocialNormalizer::normalize(vec![raw("", 100, 60)]);
        assert!(trends.is_empty());
    }

    #[test]
    fn normalize_clamps_sentiment() {
        let post = RawSocialPost {
            topic: "t".into(),
            mention_count: 10,
            rolling_window_secs: 60,
            sentiment_score: 2.5,  // out of range
            velocity: 0.0,
        };
        let trends = SocialNormalizer::normalize(vec![post]);
        assert_eq!(trends[0].sentiment, 1.0);
    }

    #[test]
    fn normalize_handles_zero_window() {
        // Zero window → clamped to 1 min to avoid div-by-zero
        let trends = SocialNormalizer::normalize(vec![raw("t", 60, 0)]);
        assert_eq!(trends.len(), 1);
        assert!((trends[0].mention_rate - 60.0).abs() < 1e-9);
    }
}
