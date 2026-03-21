// crates/data_ingestion/src/matcher/mod.rs
//
// Entity-to-market matching: cross-references entities and topics extracted
// from news items against the titles of active prediction markets.
//
// Strategy
// ────────
// For each entity/topic string in `NewsEvent::entities` and `NewsEvent::topics`,
// perform a case-insensitive substring scan over all known market titles.
// A market matches when its title contains the entity as a whole word (or token)
// to reduce false positives (e.g. "US" in "US_ELECTION" should not match
// "pause" or "because").
//
// Sentiment scoring
// ─────────────────
// A minimal positive/negative keyword counter is applied to the headline to
// produce a `sentiment_score` in [-1, 1].  An empty list means neutral (0.0).

use common::events::{MarketSnapshot, NewsEvent, SentimentUpdate};
use chrono::Utc;

// ---------------------------------------------------------------------------
// Keyword lists
// ---------------------------------------------------------------------------

const POSITIVE_WORDS: &[&str] = &[
    "win", "wins", "won", "pass", "passes", "passed", "approve", "approves",
    "approved", "rise", "rises", "increase", "increases", "up", "gain",
    "gains", "agree", "agrees", "likely", "positive", "support", "supports",
    "advance", "advances", "lead", "leads", "ahead", "victory", "confirm",
    "confirms",
];

const NEGATIVE_WORDS: &[&str] = &[
    "lose", "loses", "lost", "fail", "fails", "failed", "reject", "rejects",
    "rejected", "fall", "falls", "decrease", "decreases", "down", "drop",
    "drops", "disagree", "unlikely", "negative", "oppose", "opposes",
    "block", "blocks", "behind", "defeat", "defeats", "deny", "denied",
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute a simple keyword-based sentiment score in `[-1.0, 1.0]` from a
/// headline string.  Returns `0.0` when no keywords are found.
pub fn score_headline(headline: &str) -> f64 {
    let lower = headline.to_lowercase();
    // Tokenise on whitespace and punctuation for whole-word matching.
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphabetic())
        .filter(|t| !t.is_empty())
        .collect();

    let pos = tokens.iter().filter(|t| POSITIVE_WORDS.contains(t)).count() as f64;
    let neg = tokens.iter().filter(|t| NEGATIVE_WORDS.contains(t)).count() as f64;
    let total = pos + neg;
    if total == 0.0 {
        0.0
    } else {
        ((pos - neg) / total).clamp(-1.0, 1.0)
    }
}

/// Match a `NewsEvent` against a slice of known `MarketSnapshot`s and, if any
/// markets are relevant, return a `SentimentUpdate` carrying those market IDs
/// and a headline sentiment score.
///
/// Returns `None` when no market titles match any entity or topic.
pub fn match_news_to_markets(
    event: &NewsEvent,
    markets: &[MarketSnapshot],
) -> Option<SentimentUpdate> {
    if markets.is_empty() {
        return None;
    }

    // Build a deduplicated list of terms to search for.
    let terms: Vec<String> = event
        .entities
        .iter()
        .chain(event.topics.iter())
        .filter(|t| t.len() >= 3) // skip single-char / two-char noise
        .map(|t| t.to_lowercase())
        .collect();

    if terms.is_empty() {
        return None;
    }

    // For each market, check whether the lowercase title contains any term as a
    // whole-word match.
    let mut matched_ids: Vec<String> = Vec::new();
    for snap in markets {
        let title_lower = snap.title.to_lowercase();
        if terms.iter().any(|term| {
            // Whole-word check: preceded and followed by a non-alphanumeric char
            // (or start/end of string).
            title_lower.contains(term.as_str()) && {
                // Simple heuristic: find the byte offset and verify boundaries.
                if let Some(pos) = title_lower.find(term.as_str()) {
                    let before_ok = pos == 0
                        || !title_lower.as_bytes()[pos - 1].is_ascii_alphanumeric();
                    let after_pos = pos + term.len();
                    let after_ok = after_pos >= title_lower.len()
                        || !title_lower.as_bytes()[after_pos].is_ascii_alphanumeric();
                    before_ok && after_ok
                } else {
                    false
                }
            }
        }) {
            matched_ids.push(snap.market_id.clone());
        }
    }

    if matched_ids.is_empty() {
        return None;
    }

    let sentiment_score = score_headline(&event.headline);

    Some(SentimentUpdate {
        related_market_ids: matched_ids,
        headline: event.headline.clone(),
        sentiment_score,
        timestamp: Utc::now(),
    })
}
