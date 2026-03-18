// crates/data_ingestion/src/cache/snapshot_cache.rs
//
// In-memory snapshot cache for the data ingestion layer.
//
// Keeps the latest state of every entity type:
//   • Markets   — HashMap keyed by market_id (last-write-wins).
//   • News      — bounded VecDeque (newest at back, oldest popped from front).
//   • Social    — HashMap keyed by topic.
//   • Economic  — HashMap keyed by indicator name.
//   • Calendar  — HashMap keyed by event_id.

use std::collections::{HashMap, HashSet, VecDeque};

use common::events::{CalendarEvent, EconomicRelease, MarketSnapshot, NewsEvent, SocialTrend};

// ---------------------------------------------------------------------------
// SnapshotCache
// ---------------------------------------------------------------------------

pub struct SnapshotCache {
    // ── Markets ──────────────────────────────────────────────────────────────
    markets: HashMap<String, MarketSnapshot>,

    // ── News (bounded FIFO with deduplication) ────────────────────────────
    news: VecDeque<NewsEvent>,
    news_ids: HashSet<String>,
    max_news: usize,

    // ── Social ────────────────────────────────────────────────────────────
    trends: HashMap<String, SocialTrend>,
    max_trends: usize,

    // ── Economic ──────────────────────────────────────────────────────────
    economic: HashMap<String, EconomicRelease>,

    // ── Calendar ──────────────────────────────────────────────────────────
    calendar: HashMap<String, CalendarEvent>,
}

impl SnapshotCache {
    pub fn new(max_news: usize, max_trends: usize) -> Self {
        Self {
            markets: HashMap::new(),
            news: VecDeque::new(),
            news_ids: HashSet::new(),
            max_news,
            trends: HashMap::new(),
            max_trends,
            economic: HashMap::new(),
            calendar: HashMap::new(),
        }
    }

    // ── Market operations ──────────────────────────────────────────────────

    /// Insert or replace the snapshot for `market_id`.
    pub fn update_market(&mut self, snapshot: MarketSnapshot) {
        self.markets.insert(snapshot.market_id.clone(), snapshot);
    }

    pub fn get_market(&self, market_id: &str) -> Option<&MarketSnapshot> {
        self.markets.get(market_id)
    }

    pub fn all_markets(&self) -> impl Iterator<Item = &MarketSnapshot> {
        self.markets.values()
    }

    pub fn market_count(&self) -> usize {
        self.markets.len()
    }

    /// Snapshot of all market IDs currently tracked.
    pub fn market_ids(&self) -> Vec<String> {
        self.markets.keys().cloned().collect()
    }

    // ── News operations ───────────────────────────────────────────────────

    /// Push a news item.  Duplicate IDs (by `NewsEvent::id`) are silently dropped.
    /// When the queue is full, the oldest item is evicted.
    pub fn push_news(&mut self, event: NewsEvent) {
        if self.news_ids.contains(&event.id) {
            return; // deduplicate
        }
        if self.news.len() >= self.max_news {
            if let Some(evicted) = self.news.pop_front() {
                self.news_ids.remove(&evicted.id);
            }
        }
        self.news_ids.insert(event.id.clone());
        self.news.push_back(event);
    }

    /// Return the `limit` most recent news items (newest last in the slice).
    pub fn recent_news(&self, limit: usize) -> Vec<&NewsEvent> {
        self.news.iter().rev().take(limit).collect()
    }

    pub fn news_count(&self) -> usize {
        self.news.len()
    }

    pub fn has_news_id(&self, id: &str) -> bool {
        self.news_ids.contains(id)
    }

    // ── Social operations ─────────────────────────────────────────────────

    /// Insert or replace the trend for `topic`.  Evicts the topic with the
    /// lowest `mention_rate` when the cap is exceeded.
    pub fn update_trend(&mut self, trend: SocialTrend) {
        if !self.trends.contains_key(&trend.topic) && self.trends.len() >= self.max_trends {
            // Evict topic with lowest mention_rate
            if let Some(key) = self
                .trends
                .iter()
                .min_by(|a, b| {
                    a.1.mention_rate
                        .partial_cmp(&b.1.mention_rate)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(k, _)| k.clone())
            {
                self.trends.remove(&key);
            }
        }
        self.trends.insert(trend.topic.clone(), trend);
    }

    pub fn get_trend(&self, topic: &str) -> Option<&SocialTrend> {
        self.trends.get(topic)
    }

    pub fn all_trends(&self) -> impl Iterator<Item = &SocialTrend> {
        self.trends.values()
    }

    pub fn trend_count(&self) -> usize {
        self.trends.len()
    }

    // ── Economic operations ───────────────────────────────────────────────

    pub fn update_economic(&mut self, release: EconomicRelease) {
        self.economic.insert(release.indicator.clone(), release);
    }

    pub fn get_economic(&self, indicator: &str) -> Option<&EconomicRelease> {
        self.economic.get(indicator)
    }

    pub fn all_economic(&self) -> impl Iterator<Item = &EconomicRelease> {
        self.economic.values()
    }

    // ── Calendar operations ───────────────────────────────────────────────

    pub fn update_calendar(&mut self, event: CalendarEvent) {
        self.calendar.insert(event.event_id.clone(), event);
    }

    pub fn get_calendar_event(&self, event_id: &str) -> Option<&CalendarEvent> {
        self.calendar.get(event_id)
    }

    pub fn all_calendar_events(&self) -> impl Iterator<Item = &CalendarEvent> {
        self.calendar.values()
    }

    /// Return calendar events sorted by `scheduled_time` ascending.
    pub fn upcoming_events(&self) -> Vec<&CalendarEvent> {
        let mut events: Vec<&CalendarEvent> = self.calendar.values().collect();
        events.sort_by_key(|e| e.scheduled_time);
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn snap(id: &str, prob: f64) -> MarketSnapshot {
        MarketSnapshot {
            market_id: id.into(),
            title: "T".into(),
            description: "D".into(),
            probability: prob,
            bid: prob - 0.01,
            ask: prob + 0.01,
            volume: 1000.0,
            liquidity: 500.0,
            timestamp: Utc::now(),
        }
    }

    fn news(id: &str) -> NewsEvent {
        NewsEvent {
            id: id.into(),
            headline: "headline".into(),
            body: "body".into(),
            source: "source".into(),
            timestamp: Utc::now(),
            entities: vec![],
            topics: vec![],
        }
    }

    fn trend(topic: &str, rate: f64) -> SocialTrend {
        SocialTrend {
            topic: topic.into(),
            mention_rate: rate,
            sentiment: 0.0,
            velocity: 0.0,
            timestamp: Utc::now(),
        }
    }

    // ── Market tests ──────────────────────────────────────────────────────

    #[test]
    fn market_update_and_lookup() {
        let mut cache = SnapshotCache::new(10, 10);
        cache.update_market(snap("m1", 0.5));
        let s = cache.get_market("m1").unwrap();
        assert!((s.probability - 0.5).abs() < 1e-9);
    }

    #[test]
    fn market_update_overwrites_previous() {
        let mut cache = SnapshotCache::new(10, 10);
        cache.update_market(snap("m1", 0.4));
        cache.update_market(snap("m1", 0.7));
        assert!((cache.get_market("m1").unwrap().probability - 0.7).abs() < 1e-9);
    }

    #[test]
    fn market_count_reflects_distinct_ids() {
        let mut cache = SnapshotCache::new(10, 10);
        cache.update_market(snap("m1", 0.5));
        cache.update_market(snap("m2", 0.6));
        cache.update_market(snap("m1", 0.55)); // update, not new
        assert_eq!(cache.market_count(), 2);
    }

    // ── News tests ────────────────────────────────────────────────────────

    #[test]
    fn news_deduplication_drops_same_id() {
        let mut cache = SnapshotCache::new(10, 10);
        cache.push_news(news("n1"));
        cache.push_news(news("n1")); // duplicate
        assert_eq!(cache.news_count(), 1);
    }

    #[test]
    fn news_evicts_oldest_when_at_capacity() {
        let mut cache = SnapshotCache::new(3, 10);
        cache.push_news(news("n1"));
        cache.push_news(news("n2"));
        cache.push_news(news("n3"));
        cache.push_news(news("n4")); // should evict n1
        assert_eq!(cache.news_count(), 3);
        assert!(!cache.has_news_id("n1"), "n1 should have been evicted");
        assert!(cache.has_news_id("n4"));
    }

    #[test]
    fn recent_news_returns_newest_first() {
        let mut cache = SnapshotCache::new(10, 10);
        cache.push_news(news("n1"));
        cache.push_news(news("n2"));
        cache.push_news(news("n3"));
        let recent = cache.recent_news(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "n3"); // newest first
        assert_eq!(recent[1].id, "n2");
    }

    // ── Social tests ──────────────────────────────────────────────────────

    #[test]
    fn trend_update_replaces_existing() {
        let mut cache = SnapshotCache::new(10, 10);
        cache.update_trend(trend("t1", 10.0));
        cache.update_trend(trend("t1", 25.0));
        assert!((cache.get_trend("t1").unwrap().mention_rate - 25.0).abs() < 1e-9);
    }

    #[test]
    fn trend_evicts_lowest_rate_when_at_capacity() {
        let mut cache = SnapshotCache::new(10, 2);
        cache.update_trend(trend("low", 1.0));
        cache.update_trend(trend("high", 100.0));
        cache.update_trend(trend("new", 50.0)); // should evict "low"
        assert_eq!(cache.trend_count(), 2);
        assert!(cache.get_trend("low").is_none(), "low-rate topic should be evicted");
        assert!(cache.get_trend("high").is_some());
        assert!(cache.get_trend("new").is_some());
    }

    // ── Economic / Calendar ───────────────────────────────────────────────

    #[test]
    fn economic_update_and_lookup() {
        let mut cache = SnapshotCache::new(10, 10);
        let release = EconomicRelease {
            indicator: "US_CPI".into(),
            value: 3.1,
            forecast: Some(3.2),
            previous: Some(3.4),
            timestamp: Utc::now(),
        };
        cache.update_economic(release);
        assert!((cache.get_economic("US_CPI").unwrap().value - 3.1).abs() < 1e-9);
    }

    #[test]
    fn calendar_upcoming_sorted_by_time() {
        use chrono::Duration;
        let mut cache = SnapshotCache::new(10, 10);
        cache.update_calendar(CalendarEvent {
            event_id: "e2".into(),
            title: "Later".into(),
            scheduled_time: Utc::now() + Duration::days(10),
            category: "test".into(),
            expected_impact: 0.5,
        });
        cache.update_calendar(CalendarEvent {
            event_id: "e1".into(),
            title: "Sooner".into(),
            scheduled_time: Utc::now() + Duration::days(2),
            category: "test".into(),
            expected_impact: 0.5,
        });
        let upcoming = cache.upcoming_events();
        assert_eq!(upcoming[0].event_id, "e1");
        assert_eq!(upcoming[1].event_id, "e2");
    }
}
