// agents/graph_arb_agent/src/xplatform.rs
//
// Cross-platform arbitrage detection between Polymarket and Kalshi (or any
// two `source_platform` values).
//
// ## Strategy
//
// When a `MarketSnapshot` arrives, the agent compares the updated market's
// title against all cached snapshots that belong to a **different** platform.
// Similarity is measured by Jaccard overlap on word tokens (≥3 chars, common
// stop-words excluded).  When similarity exceeds `xplatform_min_title_similarity`
// **and** the raw probability spread exceeds fees, two `TradeSignal`s are
// emitted:
//
//   • BUY  the cheaper-priced market (market_prob is lower)
//   • SELL the more-expensive market (market_prob is higher)
//
// A per-pair cooldown (`xplatform_signal_cooldown_secs`) suppresses re-emission
// until the spread has had time to close or new price data arrives.
//
// ## Lexicographic pair key
//
// Pairs are identified by `(min(id_a, id_b), max(id_a, id_b))` so the
// cooldown map is order-independent.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use common::{events::MarketSnapshot, TradeDirection, TradeSignal};

use crate::config::GraphArbConfig;

// ---------------------------------------------------------------------------
// Stop-words excluded from Jaccard tokenisation
// ---------------------------------------------------------------------------

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "will", "who", "when", "what", "that", "this",
    "with", "are", "was", "been", "win", "wins", "won", "yes", "not",
    "get", "has", "have", "its", "from", "can", "may",
];

// ---------------------------------------------------------------------------
// Title similarity
// ---------------------------------------------------------------------------

/// Tokenise a title string into a set of lowercase alphabetic words that are
/// at least 3 characters long and not in the stop-word list.
fn tokenize(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphabetic())
        .filter(|t| t.len() >= 3 && !STOP_WORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

/// Jaccard similarity between the word-token sets of two title strings.
/// Returns a value in [0.0, 1.0]; 1.0 means identical token sets.
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.len() + tokens_b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

// ---------------------------------------------------------------------------
// Pair detection
// ---------------------------------------------------------------------------

/// A matched pair of markets across two platforms that may represent the same
/// real-world event.
#[derive(Debug, Clone)]
pub struct XPlatformPair {
    /// Market ID on the platform where the price is **lower** (BUY side).
    pub cheap_id: String,
    pub cheap_prob: f64,
    /// Market ID on the platform where the price is **higher** (SELL side).
    pub dear_id: String,
    pub dear_prob: f64,
    /// Jaccard title similarity score [0, 1].
    pub title_similarity: f64,
}

impl XPlatformPair {
    /// Canonical cooldown key: lexicographically ordered pair of IDs.
    pub fn cooldown_key(&self) -> (String, String) {
        if self.cheap_id <= self.dear_id {
            (self.cheap_id.clone(), self.dear_id.clone())
        } else {
            (self.dear_id.clone(), self.cheap_id.clone())
        }
    }
}

/// Find all cross-platform pairs involving `updated_id`.
///
/// Scans all snapshots with a different `source_platform` from `updated_id`
/// and returns those whose title similarity meets the configured threshold.
/// Both sides must have a valid, non-empty `source_platform` so that we only
/// attempt to match markets whose platform origin is known.
pub fn find_pairs_for(
    updated_id: &str,
    snapshots: &HashMap<String, MarketSnapshot>,
    min_similarity: f64,
) -> Vec<XPlatformPair> {
    let updated = match snapshots.get(updated_id) {
        Some(s) => s,
        None => return vec![],
    };

    if updated.source_platform.is_empty() {
        return vec![];
    }

    let mut pairs = Vec::new();

    for (id_b, snap_b) in snapshots {
        if id_b == updated_id {
            continue;
        }
        if snap_b.source_platform.is_empty() {
            continue;
        }
        if snap_b.source_platform == updated.source_platform {
            continue; // same platform — not a cross-platform pair
        }

        let sim = title_similarity(&updated.title, &snap_b.title);
        if sim < min_similarity {
            continue;
        }

        // Orient so `cheap` is always the lower-priced side.
        let (cheap_id, cheap_prob, dear_id, dear_prob) =
            if updated.probability <= snap_b.probability {
                (
                    updated_id.to_string(),
                    updated.probability,
                    id_b.clone(),
                    snap_b.probability,
                )
            } else {
                (
                    id_b.clone(),
                    snap_b.probability,
                    updated_id.to_string(),
                    updated.probability,
                )
            };

        pairs.push(XPlatformPair {
            cheap_id,
            cheap_prob,
            dear_id,
            dear_prob,
            title_similarity: sim,
        });
    }

    pairs
}

// ---------------------------------------------------------------------------
// Signal generation
// ---------------------------------------------------------------------------

/// Attempt to generate two `TradeSignal`s (BUY cheap + SELL dear) from a
/// cross-platform pair.  Returns an empty vec when any gate fails.
pub fn try_xplatform_signals(
    pair: &XPlatformPair,
    config: &GraphArbConfig,
    last_emitted: &HashMap<(String, String), DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Vec<TradeSignal> {
    // Gate 1: cooldown — don't re-emit the same pair too frequently.
    let key = pair.cooldown_key();
    if let Some(&last) = last_emitted.get(&key) {
        let elapsed = (now - last).num_seconds();
        if elapsed < config.xplatform_signal_cooldown_secs as i64 {
            return vec![];
        }
    }

    let spread = pair.dear_prob - pair.cheap_prob;

    // Gate 2: minimum spread.
    if spread < config.xplatform_min_spread {
        return vec![];
    }

    // Gate 3: net EV after two-sided trading costs.
    let net_ev = spread - 2.0 * config.xplatform_trading_cost;
    if net_ev <= 0.0 {
        return vec![];
    }

    // Gate 4: price bounds on both sides.
    if pair.cheap_prob < config.min_market_price || pair.dear_prob > config.max_market_price {
        return vec![];
    }

    // Confidence: edge magnitude × title similarity, capped at 1.
    let confidence =
        (spread / config.confidence_scale * pair.title_similarity).clamp(0.0, 1.0);

    // Kelly sizing per side.  We use half the spread as the per-leg edge.
    let half_edge = spread / 2.0;

    // BUY leg: buying YES on the cheap platform.
    let buy_kelly = {
        let denom = 1.0 - pair.cheap_prob;
        if denom < 1e-12 {
            return vec![];
        }
        half_edge / denom
    };
    // SELL leg: selling YES on the dear platform.
    let sell_kelly = {
        if pair.dear_prob < 1e-12 {
            return vec![];
        }
        half_edge / pair.dear_prob
    };

    let cap = config.xplatform_max_position_fraction;
    let buy_frac = (buy_kelly * config.kelly_fraction).min(cap).max(0.0);
    let sell_frac = (sell_kelly * config.kelly_fraction).min(cap).max(0.0);

    // Each signal gets half the net EV (one leg of the arb).
    let leg_ev = net_ev / 2.0;

    vec![
        TradeSignal {
            market_id: pair.cheap_id.clone(),
            direction: TradeDirection::Buy,
            expected_value: leg_ev,
            position_fraction: buy_frac,
            // We believe the fair value is near the dear platform's price.
            posterior_prob: pair.dear_prob,
            market_prob: pair.cheap_prob,
            confidence,
            timestamp: now,
            source: "cross_platform_arb".to_string(),
        },
        TradeSignal {
            market_id: pair.dear_id.clone(),
            direction: TradeDirection::Sell,
            expected_value: leg_ev,
            position_fraction: sell_frac,
            // We believe the fair value is near the cheap platform's price.
            posterior_prob: pair.cheap_prob,
            market_prob: pair.dear_prob,
            confidence,
            timestamp: now,
            source: "cross_platform_arb".to_string(),
        },
    ]
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, title: &str, platform: &str, prob: f64) -> MarketSnapshot {
        MarketSnapshot {
            market_id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            probability: prob,
            bid: prob - 0.01,
            ask: prob + 0.01,
            volume: 10_000.0,
            liquidity: 5_000.0,
            resolution_date: None,
            source_platform: platform.to_string(),
            timestamp: Utc::now(),
        }
    }

    fn cfg() -> GraphArbConfig {
        GraphArbConfig::default()
    }

    // -- title_similarity --

    #[test]
    fn identical_titles_give_similarity_one() {
        assert!((title_similarity("Trump wins election", "Trump wins election") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn completely_different_titles_give_zero() {
        assert_eq!(title_similarity("Bitcoin price", "Euro cup soccer"), 0.0);
    }

    #[test]
    fn partial_overlap_between_zero_and_one() {
        let sim = title_similarity(
            "Will Trump win the 2024 presidential election",
            "Trump 2024 election winner",
        );
        assert!(sim > 0.0 && sim < 1.0, "sim={sim}");
    }

    // -- find_pairs_for --

    #[test]
    fn no_pairs_when_same_platform() {
        let mut snaps = HashMap::new();
        snaps.insert("A".into(), snap("A", "Trump wins 2024 election", "polymarket", 0.42));
        snaps.insert("B".into(), snap("B", "Trump wins 2024 election", "polymarket", 0.46));
        let pairs = find_pairs_for("A", &snaps, 0.40);
        assert!(pairs.is_empty(), "should not match same platform");
    }

    #[test]
    fn no_pairs_when_empty_platform() {
        let mut snaps = HashMap::new();
        snaps.insert("A".into(), snap("A", "Trump wins 2024 election", "", 0.42));
        snaps.insert("B".into(), snap("B", "Trump wins 2024 election", "kalshi", 0.46));
        let pairs = find_pairs_for("A", &snaps, 0.40);
        assert!(pairs.is_empty(), "empty platform should be skipped");
    }

    #[test]
    fn finds_pair_across_platforms_high_similarity() {
        let mut snaps = HashMap::new();
        snaps.insert(
            "POLY-A".into(),
            snap("POLY-A", "Will Trump win the 2024 presidential election", "polymarket", 0.42),
        );
        snaps.insert(
            "KAL-B".into(),
            snap("KAL-B", "Trump 2024 presidential election victory", "kalshi", 0.46),
        );
        let pairs = find_pairs_for("POLY-A", &snaps, 0.20);
        assert_eq!(pairs.len(), 1);
        // cheap side is POLY-A (0.42 < 0.46)
        assert_eq!(pairs[0].cheap_id, "POLY-A");
        assert_eq!(pairs[0].dear_id, "KAL-B");
        assert!((pairs[0].cheap_prob - 0.42).abs() < 1e-9);
        assert!((pairs[0].dear_prob - 0.46).abs() < 1e-9);
    }

    #[test]
    fn no_pair_when_similarity_below_threshold() {
        let mut snaps = HashMap::new();
        snaps.insert("A".into(), snap("A", "Bitcoin price 2025", "polymarket", 0.42));
        snaps.insert("B".into(), snap("B", "Euro cup soccer winner", "kalshi", 0.46));
        let pairs = find_pairs_for("A", &snaps, 0.40);
        assert!(pairs.is_empty());
    }

    // -- try_xplatform_signals --

    fn matching_pair() -> XPlatformPair {
        XPlatformPair {
            cheap_id: "POLY-A".into(),
            cheap_prob: 0.42,
            dear_id: "KAL-B".into(),
            dear_prob: 0.46,
            title_similarity: 0.60,
        }
    }

    #[test]
    fn profitable_pair_emits_two_signals() {
        let pair = matching_pair();
        let last_emitted = HashMap::new();
        let signals = try_xplatform_signals(&pair, &cfg(), &last_emitted, Utc::now());
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].market_id, "POLY-A");
        assert_eq!(signals[0].direction, TradeDirection::Buy);
        assert_eq!(signals[1].market_id, "KAL-B");
        assert_eq!(signals[1].direction, TradeDirection::Sell);
        assert!(signals[0].expected_value > 0.0);
        assert!(signals[1].expected_value > 0.0);
        assert!(signals[0].position_fraction > 0.0);
        assert!(signals[0].position_fraction <= 0.03 + 1e-9);
    }

    #[test]
    fn spread_below_minimum_emits_nothing() {
        let pair = XPlatformPair {
            cheap_id: "A".into(),
            cheap_prob: 0.50,
            dear_id: "B".into(),
            dear_prob: 0.51, // spread=0.01 < default min_spread=0.02
            title_similarity: 0.80,
        };
        let signals = try_xplatform_signals(&pair, &cfg(), &HashMap::new(), Utc::now());
        assert!(signals.is_empty(), "spread too small should not emit");
    }

    #[test]
    fn spread_eaten_by_costs_emits_nothing() {
        // spread=0.02, costs=2×0.008=0.016, net_ev=0.004 > 0 — actually this should pass.
        // Use a higher cost config to force net_ev ≤ 0.
        let c = GraphArbConfig {
            xplatform_trading_cost: 0.015, // 2×0.015=0.030 > spread=0.02
            ..GraphArbConfig::default()
        };
        let pair = XPlatformPair {
            cheap_id: "A".into(),
            cheap_prob: 0.44,
            dear_id: "B".into(),
            dear_prob: 0.46, // spread=0.02
            title_similarity: 0.80,
        };
        let signals = try_xplatform_signals(&pair, &c, &HashMap::new(), Utc::now());
        assert!(signals.is_empty(), "costs exceed spread should not emit");
    }

    #[test]
    fn cooldown_suppresses_repeat_emission() {
        let pair = matching_pair();
        let mut last_emitted = HashMap::new();
        let now = Utc::now();
        // Record a very recent emission.
        last_emitted.insert(pair.cooldown_key(), now);
        let signals = try_xplatform_signals(&pair, &cfg(), &last_emitted, now);
        assert!(signals.is_empty(), "should be suppressed during cooldown");
    }

    #[test]
    fn cooldown_expires_allows_re_emission() {
        let pair = matching_pair();
        let mut last_emitted = HashMap::new();
        let now = Utc::now();
        // Record an old emission (2 minutes ago > default 60 s cooldown).
        let old = now - chrono::Duration::seconds(120);
        last_emitted.insert(pair.cooldown_key(), old);
        let signals = try_xplatform_signals(&pair, &cfg(), &last_emitted, now);
        assert_eq!(signals.len(), 2, "should re-emit after cooldown expires");
    }
}
