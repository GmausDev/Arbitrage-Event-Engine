// crates/data_ingestion/src/connectors/mod.rs
//
// Raw API response types shared by all connectors, plus sub-module declarations.

pub mod calendar_connectors;
pub mod economic_connectors;
pub mod market_connectors;
pub mod news_connectors;
pub mod social_connectors;

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Raw market data (pre-normalisation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawMarket {
    pub market_id: String,
    pub title: String,
    pub description: String,
    pub probability: f64,
    pub bid: f64,
    pub ask: f64,
    pub volume: f64,
    pub liquidity: f64,
}

#[derive(Debug, Clone)]
pub struct RawOrderBook {
    pub market_id: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_depth: f64,
    pub ask_depth: f64,
}

#[derive(Debug, Clone)]
pub struct RawTrade {
    pub market_id: String,
    pub price: f64,
    pub size: f64,
    /// "buy" or "sell"
    pub side: String,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Raw news data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawNewsItem {
    pub id: String,
    pub headline: String,
    pub body: String,
    pub source: String,
    pub entities: Vec<String>,
    pub topics: Vec<String>,
    pub published_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Raw social data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawSocialPost {
    pub topic: String,
    pub mention_count: u64,
    pub rolling_window_secs: u64,
    pub sentiment_score: f64,
    pub velocity: f64,
}

// ---------------------------------------------------------------------------
// Raw economic data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawEconomicData {
    pub indicator: String,
    pub value: f64,
    pub forecast: Option<f64>,
    pub previous: Option<f64>,
    pub release_time: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Raw calendar data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawCalendarEntry {
    pub event_id: String,
    pub title: String,
    pub scheduled_time: DateTime<Utc>,
    pub category: String,
    pub expected_impact: f64,
}
