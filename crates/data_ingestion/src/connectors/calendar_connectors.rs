// crates/data_ingestion/src/connectors/calendar_connectors.rs

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration, Utc};

use super::RawCalendarEntry;

#[async_trait]
pub trait CalendarConnector: Send + Sync {
    fn name(&self) -> &str;
    /// Fetch upcoming scheduled events within the next `lookahead_days`.
    async fn fetch_events(&self, lookahead_days: u32) -> Result<Vec<RawCalendarEntry>>;
}

// ---------------------------------------------------------------------------
// PredictIt event calendar stub
// ---------------------------------------------------------------------------

pub struct PredictItCalendarConnector;

impl PredictItCalendarConnector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CalendarConnector for PredictItCalendarConnector {
    fn name(&self) -> &str {
        "predictit_calendar"
    }

    async fn fetch_events(&self, lookahead_days: u32) -> Result<Vec<RawCalendarEntry>> {
        let horizon = Utc::now() + Duration::days(lookahead_days as i64);
        Ok(vec![
            RawCalendarEntry {
                event_id: "fomc_mar_2025".into(),
                title: "FOMC Meeting — March 2025 Rate Decision".into(),
                scheduled_time: Utc::now() + Duration::days(15),
                category: "central_bank".into(),
                expected_impact: 0.82,
            },
            RawCalendarEntry {
                event_id: "us_election_nov_2026".into(),
                title: "US Midterm Elections 2026".into(),
                scheduled_time: Utc::now() + Duration::days(600),
                category: "election".into(),
                expected_impact: 0.95,
            },
        ]
        .into_iter()
        .filter(|e| e.scheduled_time <= horizon)
        .collect())
    }
}

// ---------------------------------------------------------------------------
// Earnings calendar stub
// ---------------------------------------------------------------------------

pub struct EarningsCalendarConnector;

impl EarningsCalendarConnector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CalendarConnector for EarningsCalendarConnector {
    fn name(&self) -> &str {
        "earnings_calendar"
    }

    async fn fetch_events(&self, lookahead_days: u32) -> Result<Vec<RawCalendarEntry>> {
        let horizon = Utc::now() + Duration::days(lookahead_days as i64);
        Ok(vec![
            RawCalendarEntry {
                event_id: "aapl_q1_2025_earnings".into(),
                title: "Apple Q1 2025 Earnings Call".into(),
                scheduled_time: Utc::now() + Duration::days(8),
                category: "earnings".into(),
                expected_impact: 0.55,
            },
            RawCalendarEntry {
                event_id: "nvda_q4_2025_earnings".into(),
                title: "NVIDIA Q4 2025 Earnings Call".into(),
                scheduled_time: Utc::now() + Duration::days(21),
                category: "earnings".into(),
                expected_impact: 0.72,
            },
        ]
        .into_iter()
        .filter(|e| e.scheduled_time <= horizon)
        .collect())
    }
}

// ---------------------------------------------------------------------------
// Mock (tests)
// ---------------------------------------------------------------------------

pub struct MockCalendarConnector {
    pub entries: Vec<RawCalendarEntry>,
}

impl MockCalendarConnector {
    pub fn new(entries: Vec<RawCalendarEntry>) -> Self {
        Self { entries }
    }
}

#[async_trait]
impl CalendarConnector for MockCalendarConnector {
    fn name(&self) -> &str {
        "mock_calendar"
    }

    async fn fetch_events(&self, _lookahead_days: u32) -> Result<Vec<RawCalendarEntry>> {
        Ok(self.entries.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn predictit_calendar_filters_by_horizon() {
        let c = PredictItCalendarConnector;
        // lookahead = 30 days: only the FOMC event (day 15) should fit; not the election (day 600)
        let events = c.fetch_events(30).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "fomc_mar_2025");
    }

    #[tokio::test]
    async fn predictit_calendar_long_horizon_returns_both() {
        let c = PredictItCalendarConnector;
        let events = c.fetch_events(1000).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn mock_calendar_returns_preset() {
        let entry = RawCalendarEntry {
            event_id: "e1".into(),
            title: "Test Event".into(),
            scheduled_time: Utc::now() + Duration::days(1),
            category: "test".into(),
            expected_impact: 0.5,
        };
        let c = MockCalendarConnector::new(vec![entry]);
        let result = c.fetch_events(30).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].event_id, "e1");
    }
}
