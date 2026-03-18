// crates/data_ingestion/src/normalization/market_normalizer.rs
//
// Converts `RawMarket` / `RawOrderBook` → `MarketSnapshot` + `MarketUpdate`.

use chrono::Utc;
use common::{
    events::{MarketSnapshot, MarketUpdate},
    types::MarketNode,
};

use crate::connectors::{RawMarket, RawOrderBook};

pub struct MarketNormalizer;

impl MarketNormalizer {
    /// Convert a batch of `RawMarket` values into `MarketSnapshot` events.
    ///
    /// Invalid entries (probability out of [0,1]) are discarded with a warning.
    pub fn normalize_markets(raw: Vec<RawMarket>) -> Vec<MarketSnapshot> {
        raw.into_iter()
            .filter_map(|r| {
                let prob = r.probability.clamp(0.0, 1.0);
                let bid = r.bid.clamp(0.0, 1.0);
                let ask = r.ask.clamp(bid, 1.0); // ensure ask >= bid
                if r.market_id.is_empty() {
                    tracing::warn!("market_normalizer: skipping market with empty id");
                    return None;
                }
                Some(MarketSnapshot {
                    market_id: r.market_id,
                    title: r.title,
                    description: r.description,
                    probability: prob,
                    bid,
                    ask,
                    volume: r.volume.max(0.0),
                    liquidity: r.liquidity.max(0.0),
                    timestamp: Utc::now(),
                })
            })
            .collect()
    }

    /// Convert `MarketSnapshot`s into lightweight `MarketUpdate` events for
    /// backward compatibility with `market_graph` and `bayesian_engine`.
    pub fn to_market_updates(snapshots: &[MarketSnapshot]) -> Vec<MarketUpdate> {
        snapshots
            .iter()
            .map(|s| MarketUpdate {
                market: MarketNode {
                    id: s.market_id.clone(),
                    probability: s.probability,
                    liquidity: s.liquidity,
                    last_update: s.timestamp,
                },
            })
            .collect()
    }

    /// Merge order-book data into existing snapshots (updates bid/ask in place).
    pub fn apply_orderbooks(
        mut snapshots: Vec<MarketSnapshot>,
        books: Vec<RawOrderBook>,
    ) -> Vec<MarketSnapshot> {
        for book in books {
            if let Some(snap) = snapshots
                .iter_mut()
                .find(|s| s.market_id == book.market_id)
            {
                snap.bid = book.best_bid.clamp(0.0, 1.0);
                snap.ask = book.best_ask.clamp(snap.bid, 1.0);
                // Recalculate mid from order book
                snap.probability = ((snap.bid + snap.ask) / 2.0).clamp(0.0, 1.0);
            }
        }
        snapshots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::RawMarket;

    fn raw(id: &str, prob: f64, bid: f64, ask: f64) -> RawMarket {
        RawMarket {
            market_id: id.into(),
            title: "T".into(),
            description: "D".into(),
            probability: prob,
            bid,
            ask,
            volume: 100.0,
            liquidity: 50.0,
        }
    }

    #[test]
    fn normalize_clamps_probability() {
        let snapshots = MarketNormalizer::normalize_markets(vec![
            raw("m1", 1.5, 0.49, 0.51),  // prob > 1 → clamped to 1.0
            raw("m2", -0.1, 0.0, 0.01),  // prob < 0 → clamped to 0.0
        ]);
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].probability, 1.0);
        assert_eq!(snapshots[1].probability, 0.0);
    }

    #[test]
    fn normalize_discards_empty_id() {
        let snapshots =
            MarketNormalizer::normalize_markets(vec![raw("", 0.5, 0.49, 0.51)]);
        assert!(snapshots.is_empty());
    }

    #[test]
    fn normalize_ensures_ask_gte_bid() {
        let snapshots =
            MarketNormalizer::normalize_markets(vec![raw("m1", 0.5, 0.55, 0.40)]);
        // ask was below bid → ask should be clamped to bid
        assert!(snapshots[0].ask >= snapshots[0].bid);
    }

    #[test]
    fn to_market_updates_preserves_id_and_probability() {
        let snaps = MarketNormalizer::normalize_markets(vec![raw("m1", 0.65, 0.64, 0.66)]);
        let updates = MarketNormalizer::to_market_updates(&snaps);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].market.id, "m1");
        assert!((updates[0].market.probability - 0.65).abs() < 1e-9);
    }

    #[test]
    fn apply_orderbooks_updates_bid_ask() {
        let snaps = MarketNormalizer::normalize_markets(vec![raw("m1", 0.5, 0.49, 0.51)]);
        let updated = MarketNormalizer::apply_orderbooks(
            snaps,
            vec![RawOrderBook {
                market_id: "m1".into(),
                best_bid: 0.53,
                best_ask: 0.57,
                bid_depth: 1000.0,
                ask_depth: 800.0,
            }],
        );
        assert!((updated[0].bid - 0.53).abs() < 1e-9);
        assert!((updated[0].ask - 0.57).abs() < 1e-9);
        // Mid should be recomputed
        assert!((updated[0].probability - 0.55).abs() < 1e-9);
    }
}
