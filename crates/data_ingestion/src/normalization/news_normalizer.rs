// crates/data_ingestion/src/normalization/news_normalizer.rs

use common::events::NewsEvent;

use crate::connectors::RawNewsItem;

pub struct NewsNormalizer;

impl NewsNormalizer {
    /// Convert raw news items into `NewsEvent` payloads.
    ///
    /// Items with empty headlines or IDs are discarded.
    pub fn normalize(raw: Vec<RawNewsItem>) -> Vec<NewsEvent> {
        raw.into_iter()
            .filter_map(|r| {
                if r.id.is_empty() || r.headline.is_empty() {
                    tracing::warn!("news_normalizer: skipping item with empty id or headline");
                    return None;
                }
                Some(NewsEvent {
                    id: r.id,
                    headline: r.headline,
                    body: r.body,
                    source: r.source,
                    timestamp: r.published_at,
                    entities: r.entities,
                    topics: r.topics,
                })
            })
            .collect()
    }

    /// Deduplicate a list of `NewsEvent`s by `id`, keeping first occurrence.
    pub fn dedup(events: Vec<NewsEvent>) -> Vec<NewsEvent> {
        let mut seen = std::collections::HashSet::new();
        events
            .into_iter()
            .filter(|e| seen.insert(e.id.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn raw(id: &str, headline: &str) -> RawNewsItem {
        RawNewsItem {
            id: id.into(),
            headline: headline.into(),
            body: "body".into(),
            source: "source".into(),
            entities: vec!["entity_a".into()],
            topics: vec!["topic_x".into()],
            published_at: Utc::now(),
        }
    }

    #[test]
    fn normalize_discards_empty_id() {
        let result = NewsNormalizer::normalize(vec![raw("", "headline")]);
        assert!(result.is_empty());
    }

    #[test]
    fn normalize_discards_empty_headline() {
        let result = NewsNormalizer::normalize(vec![raw("id1", "")]);
        assert!(result.is_empty());
    }

    #[test]
    fn normalize_preserves_entities_and_topics() {
        let result = NewsNormalizer::normalize(vec![raw("id1", "headline")]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].entities, vec!["entity_a"]);
        assert_eq!(result[0].topics, vec!["topic_x"]);
    }

    #[test]
    fn dedup_removes_duplicates() {
        let events = NewsNormalizer::normalize(vec![
            raw("id1", "first"),
            raw("id1", "duplicate"),
            raw("id2", "second"),
        ]);
        let deduped = NewsNormalizer::dedup(events);
        assert_eq!(deduped.len(), 2);
        // First occurrence preserved
        assert_eq!(deduped[0].headline, "first");
    }
}
