// crates/relationship_discovery/src/correlator.rs
//
// Pure statistical and semantic computations — no I/O, no async, no locks.
//
// ## Subsystems implemented here
//
// 1. Historical Correlation Engine  — Pearson + Spearman r
// 2. Mutual Information Engine      — discrete binning, normalised MI
// 3. Conditional Probability        — P(A_up|B_up), directional inference
// 4. Market Embedding Engine        — TF-IDF token similarity
// 5. Relationship Scoring Engine    — weighted composite score + filter

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use common::{EdgeDirection, RelationshipDiscovered};

use crate::{
    config::RelationshipDiscoveryConfig,
    types::{JointCounts, JointEntry, MarketPair, MoveState},
};

// ===========================================================================
// 1. Historical Correlation Engine
// ===========================================================================

/// Pearson correlation coefficient over two equal-length slices.
/// Returns `None` when n < 2 or either series has zero variance.
pub fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len().min(ys.len());
    if n < 2 {
        return None;
    }
    // Use the most-recent n samples from each (trailing alignment).
    let xs = &xs[xs.len() - n..];
    let ys = &ys[ys.len() - n..];

    let nf = n as f64;
    let mx = xs.iter().sum::<f64>() / nf;
    let my = ys.iter().sum::<f64>() / nf;

    let (mut cov, mut vx, mut vy) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in xs.iter().zip(ys) {
        let dx = x - mx;
        let dy = y - my;
        cov += dx * dy;
        vx  += dx * dx;
        vy  += dy * dy;
    }
    let denom = (vx * vy).sqrt();
    // Use `!(> threshold)` rather than `< threshold` so that NaN also returns
    // None — `NaN > x` is always false, so `!(NaN > 1e-12)` is true.
    if !(denom > 1e-12) {
        return None;
    }
    Some((cov / denom).clamp(-1.0, 1.0))
}

/// Convert a slice to its rank vector (1-based average ranks for ties).
fn ranks(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut idx: Vec<(usize, f64)> = xs.iter().copied().enumerate().collect();
    // `total_cmp` gives a total order (NaN sorts after Inf) and never panics.
    idx.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut r = vec![0.0f64; n];
    let mut i = 0;
    while i < n {
        let v = idx[i].1;
        let mut j = i + 1;
        // Epsilon comparison prevents floating-point representation artefacts
        // from assigning different ranks to values that are mathematically equal.
        while j < n && (idx[j].1 - v).abs() < 1e-12 {
            j += 1;
        }
        let avg = (i + j + 1) as f64 / 2.0; // 1-based
        for k in i..j {
            r[idx[k].0] = avg;
        }
        i = j;
    }
    r
}

/// Spearman rank-correlation coefficient.
/// Returns `None` when n < 2 or rank variance is zero.
pub fn spearman(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len().min(ys.len());
    if n < 2 {
        return None;
    }
    let rx = ranks(&xs[xs.len() - n..]);
    let ry = ranks(&ys[ys.len() - n..]);
    pearson(&rx, &ry)
}

// ===========================================================================
// 2. Mutual Information Engine
// ===========================================================================

/// Normalised mutual information ∈ [0, 1] from joint move-state counts.
///
/// Returns 0.0 when there are fewer than `min_n` observations.
/// Uses `MI / min(H(A), H(B))` normalisation so the result is bounded.
pub fn normalized_mi(jc: &JointCounts, min_n: usize) -> f64 {
    let total: u64 = jc.counts.values().sum();
    if (total as usize) < min_n {
        return 0.0;
    }
    let tf = total as f64;

    let mut marg_a: HashMap<MoveState, f64> = HashMap::new();
    let mut marg_b: HashMap<MoveState, f64> = HashMap::new();
    for (&(sa, sb), &cnt) in &jc.counts {
        *marg_a.entry(sa).or_default() += cnt as f64;
        *marg_b.entry(sb).or_default() += cnt as f64;
    }

    let entropy = |m: &HashMap<MoveState, f64>| -> f64 {
        m.values()
            .map(|&c| {
                let p = c / tf;
                if p < 1e-12 { 0.0 } else { -p * p.ln() }
            })
            .sum()
    };
    let h_a = entropy(&marg_a);
    let h_b = entropy(&marg_b);

    let mut mi = 0.0f64;
    for (&(sa, sb), &cnt) in &jc.counts {
        if cnt == 0 {
            continue;
        }
        let p_ab = cnt as f64 / tf;
        let p_a  = marg_a[&sa]  / tf;
        let p_b  = marg_b[&sb]  / tf;
        let d    = p_a * p_b;
        if d < 1e-12 {
            continue;
        }
        mi += p_ab * (p_ab / d).ln();
    }
    mi = mi.max(0.0); // clamp numerical noise

    let h_min = h_a.min(h_b);
    if h_min < 1e-12 { 0.0 } else { (mi / h_min).clamp(0.0, 1.0) }
}

// ===========================================================================
// 3. Conditional Probability Estimator
// ===========================================================================

/// Returns (P(A_up | B_up), P(A_down | B_down)) — co-movement conditionals.
///
/// High values in both indicate that market B conditionally leads market A
/// (i.e., B → A, `EdgeDirection::BtoA`).
/// Returns `None` when fewer than `min_n` total observations are available.
pub fn directional_conditionals(jc: &JointCounts, min_n: usize) -> Option<(f64, f64)> {
    let total: u64 = jc.counts.values().sum();
    if (total as usize) < min_n {
        return None;
    }

    let count = |sa: MoveState, sb: MoveState| -> f64 {
        *jc.counts.get(&(sa, sb)).unwrap_or(&0) as f64
    };

    // Marginal counts for B (the conditioning variable).
    let b_up   = count(MoveState::Up,   MoveState::Up)
               + count(MoveState::Down, MoveState::Up)
               + count(MoveState::Flat, MoveState::Up);
    let b_down = count(MoveState::Up,   MoveState::Down)
               + count(MoveState::Down, MoveState::Down)
               + count(MoveState::Flat, MoveState::Down);

    let p_a_up_gvn_b_up   = if b_up   > 0.0 { (count(MoveState::Up,   MoveState::Up)   / b_up).clamp(0.0, 1.0) } else { 0.0 };
    let p_a_down_gvn_b_dn = if b_down > 0.0 { (count(MoveState::Down, MoveState::Down) / b_down).clamp(0.0, 1.0) } else { 0.0 };

    Some((p_a_up_gvn_b_up, p_a_down_gvn_b_dn))
}

/// Returns (P(B_up | A_up), P(B_down | A_down)) — the reverse conditionals.
///
/// High values indicate that market A conditionally leads market B
/// (i.e., A → B, `EdgeDirection::AtoB`).
pub fn directional_conditionals_reverse(jc: &JointCounts, min_n: usize) -> Option<(f64, f64)> {
    let total: u64 = jc.counts.values().sum();
    if (total as usize) < min_n {
        return None;
    }

    let count = |sa: MoveState, sb: MoveState| -> f64 {
        *jc.counts.get(&(sa, sb)).unwrap_or(&0) as f64
    };

    // Marginal counts for A (the conditioning variable in the reverse case).
    let a_up   = count(MoveState::Up, MoveState::Up)
               + count(MoveState::Up, MoveState::Down)
               + count(MoveState::Up, MoveState::Flat);
    let a_down = count(MoveState::Down, MoveState::Up)
               + count(MoveState::Down, MoveState::Down)
               + count(MoveState::Down, MoveState::Flat);

    let p_b_up_gvn_a_up   = if a_up   > 0.0 { (count(MoveState::Up,   MoveState::Up)   / a_up).clamp(0.0, 1.0) } else { 0.0 };
    let p_b_down_gvn_a_dn = if a_down > 0.0 { (count(MoveState::Down, MoveState::Down) / a_down).clamp(0.0, 1.0) } else { 0.0 };

    Some((p_b_up_gvn_a_up, p_b_down_gvn_a_dn))
}

/// Infer edge directionality from joint state counts.
fn infer_direction(jc: &JointCounts, config: &RelationshipDiscoveryConfig) -> EdgeDirection {
    let t = config.directed_threshold;

    // Does A follow B?
    let a_follows_b = directional_conditionals(jc, config.min_history_len)
        .map(|(up, dn)| up >= t && dn >= t)
        .unwrap_or(false);

    // Does B follow A?
    let b_follows_a = directional_conditionals_reverse(jc, config.min_history_len)
        .map(|(up, dn)| up >= t && dn >= t)
        .unwrap_or(false);

    match (a_follows_b, b_follows_a) {
        (true, false)  => EdgeDirection::BtoA, // B drives A
        (false, true)  => EdgeDirection::AtoB, // A drives B
        _              => EdgeDirection::Undirected,
    }
}

// ===========================================================================
// 4. Market Embedding Engine
// ===========================================================================
//
// `MarketNode` carries only a market ID string (no free-text description).
// We tokenize the ID by splitting on common delimiters and build a
// unit-L2-norm TF-IDF weight vector.  Cosine similarity between two such
// vectors approximates semantic relatedness.
//
// When market titles / descriptions become available in `MarketNode`,
// they can be substituted as the tokenization source with no changes
// to the downstream scoring logic.

/// Tokenize a market ID into lowercase terms, filtering single-char tokens.
pub fn tokenize(market_id: &str) -> Vec<String> {
    market_id
        .split(|c: char| matches!(c, '-' | '_' | '/' | ':' | ' '))
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .collect()
}

/// Build a unit-L2-norm TF-weight vector, sorted by token for merge-join.
pub fn build_embedding(tokens: &[String]) -> Vec<(String, f64)> {
    if tokens.is_empty() {
        return vec![];
    }
    let total = tokens.len() as f64;
    let mut tf: HashMap<String, f64> = HashMap::new();
    for t in tokens {
        *tf.entry(t.clone()).or_default() += 1.0 / total;
    }
    let l2: f64 = tf.values().map(|v| v * v).sum::<f64>().sqrt();
    if l2 < 1e-12 {
        return vec![];
    }
    let mut v: Vec<(String, f64)> = tf.into_iter().map(|(t, w)| (t, w / l2)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Cosine similarity between two sorted TF-IDF vectors ∈ [0, 1].
///
/// Both slices must be sorted by token string for the merge-join to be correct.
pub fn cosine_similarity(a: &[(String, f64)], b: &[(String, f64)]) -> f64 {
    let (mut i, mut j, mut dot) = (0usize, 0usize, 0.0f64);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Equal   => { dot += a[i].1 * b[j].1; i += 1; j += 1; }
            std::cmp::Ordering::Less    => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    dot.clamp(0.0, 1.0)
}

// ===========================================================================
// 5. Relationship Scoring Engine + 6. Relationship Filter
// ===========================================================================

/// Attempt to construct a `RelationshipDiscovered` from a joint entry.
///
/// Applies all gates in order:
///   1. Minimum paired observations (`min_history_len`)
///   2. Deduplication window (`edge_ttl_secs`)
///   3. Composite score threshold (`min_relationship_score`)
///
/// On success, sets `joint.last_emitted_at = Some(now)` before returning,
/// so the caller does not need a second write to record the emission.
///
/// `emb_a` and `emb_b` are the pre-computed sorted embedding vectors for the
/// canonical market_a and market_b respectively.
pub fn try_relationship(
    pair:   &MarketPair,
    joint:  &mut JointEntry,
    config: &RelationshipDiscoveryConfig,
    emb_a:  &[(String, f64)],
    emb_b:  &[(String, f64)],
    now:    DateTime<Utc>,
) -> Option<RelationshipDiscovered> {
    // ── Gate 1: minimum observations ────────────────────────────────────────
    let n = joint.pairs.len();
    if n < config.min_history_len {
        return None;
    }

    // ── Gate 2: deduplication window ────────────────────────────────────────
    // Use signed arithmetic so a backward clock jump (negative elapsed) is
    // treated as "not yet expired" rather than an enormous unsigned age.
    if let Some(last) = joint.last_emitted_at {
        let elapsed = (now - last).num_seconds();
        if elapsed >= 0 && (elapsed as u64) < config.edge_ttl_secs {
            return None;
        }
    }

    // ── Correlation (average of |Pearson| and |Spearman|) ───────────────────
    // Strip the classification field — we only need the raw prices here.
    let (xs, ys): (Vec<f64>, Vec<f64>) =
        joint.pairs.iter().map(|&(a, b, _)| (a, b)).unzip();

    // Compute first-differences (returns) before correlating.
    //
    // Prediction market prices all converge to 0 or 1 at resolution, so
    // levels correlation is spuriously inflated for any two markets that
    // happen to trend in the same direction.  Returns correlation captures
    // genuine co-movement (both reacted similarly to the same information)
    // rather than shared terminal drift.
    let rx: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
    let ry: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();

    // unwrap_or(0.0): zero-variance returns means "not correlated", not
    // "insufficient data" (the min_history_len gate above already ensures
    // we have enough paired observations).
    let p = pearson(&rx, &ry).unwrap_or(0.0);
    let s = spearman(&rx, &ry).unwrap_or(0.0);
    let corr_combined = (p.abs() + s.abs()) / 2.0;

    // ── Mutual information ───────────────────────────────────────────────────
    let mi = normalized_mi(&joint.joint_counts, config.min_history_len);

    // ── Embedding similarity ─────────────────────────────────────────────────
    let emb_sim = cosine_similarity(emb_a, emb_b);

    // ── Gate 3: composite score ──────────────────────────────────────────────
    let strength = (config.correlation_weight   * corr_combined
                  + config.mutual_info_weight   * mi
                  + config.embedding_weight     * emb_sim)
                  .clamp(0.0, 1.0);

    if strength < config.min_relationship_score {
        return None;
    }

    // ── NaN safety ───────────────────────────────────────────────────────────
    // A NaN probability input can propagate through the math and reach here
    // as a NaN strength.  Reject rather than publish a corrupt event.
    if strength.is_nan() || p.is_nan() || mi.is_nan() {
        return None;
    }

    // ── Directionality ───────────────────────────────────────────────────────
    let direction = infer_direction(&joint.joint_counts, config);

    // Record emission time so the next call respects the TTL.
    joint.last_emitted_at = Some(now);

    Some(RelationshipDiscovered {
        market_a:            pair.0.clone(),
        market_b:            pair.1.clone(),
        strength,
        direction,
        correlation:         p.abs(),
        mutual_information:  mi,
        embedding_similarity: emb_sim,
        sample_count:        n,
        timestamp:           now,
    })
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{JointCounts, JointEntry, MoveState};
    // ── Helpers ──────────────────────────────────────────────────────────────

    fn test_config() -> RelationshipDiscoveryConfig {
        RelationshipDiscoveryConfig {
            min_history_len: 5,
            min_relationship_score: 0.35,
            directed_threshold: 0.65,
            edge_ttl_secs: 0,
            ..Default::default()
        }
    }

    /// Build a `JointEntry` from parallel price vectors.
    fn joint_from(pairs: Vec<(f64, f64)>, threshold: f64) -> JointEntry {
        let mut e = JointEntry::default();
        for (pa, pb) in pairs {
            e.push(pa, pb, 200, threshold);
        }
        e
    }

    /// Build a `JointCounts` from raw (MoveState, MoveState) → count entries.
    fn counts_from(entries: Vec<((MoveState, MoveState), u64)>) -> JointCounts {
        JointCounts {
            counts: entries.into_iter().collect(),
        }
    }

    // ── 1. Pearson correlation ────────────────────────────────────────────────

    #[test]
    fn pearson_perfect_positive() {
        let xs = [0.1, 0.2, 0.3, 0.4, 0.5];
        let ys = [0.2, 0.4, 0.6, 0.8, 1.0];
        let r = pearson(&xs, &ys).unwrap();
        assert!((r - 1.0).abs() < 1e-9, "expected r=1.0, got {r}");
    }

    #[test]
    fn pearson_perfect_negative() {
        let xs = [0.1, 0.2, 0.3, 0.4, 0.5];
        let ys = [1.0, 0.8, 0.6, 0.4, 0.2];
        let r = pearson(&xs, &ys).unwrap();
        assert!((r + 1.0).abs() < 1e-9, "expected r=-1.0, got {r}");
    }

    #[test]
    fn pearson_constant_series_returns_none() {
        let xs = [0.5, 0.5, 0.5, 0.5];
        let ys = [0.1, 0.2, 0.3, 0.4];
        assert!(pearson(&xs, &ys).is_none());
    }

    #[test]
    fn pearson_too_short_returns_none() {
        assert!(pearson(&[0.5], &[0.5]).is_none());
    }

    // ── 2. Spearman correlation ───────────────────────────────────────────────

    #[test]
    fn spearman_monotone_positive() {
        // Any strictly monotone series → Spearman = 1.0
        let xs = [1.0, 3.0, 7.0, 10.0, 20.0];
        let ys = [2.0, 4.0, 8.0, 12.0, 25.0];
        let r = spearman(&xs, &ys).unwrap();
        assert!((r - 1.0).abs() < 1e-9, "expected Spearman=1.0, got {r}");
    }

    #[test]
    fn spearman_monotone_negative() {
        let xs = [1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = [5.0, 4.0, 3.0, 2.0, 1.0];
        let r = spearman(&xs, &ys).unwrap();
        assert!((r + 1.0).abs() < 1e-9, "expected Spearman=-1.0, got {r}");
    }

    // ── 3. Mutual information ─────────────────────────────────────────────────

    #[test]
    fn mi_perfect_correlation_gives_nonzero() {
        // When A and B always move in the same direction (Up or Down with equal
        // frequency), their joint distribution has non-zero entropy and MI = 1.0.
        // A single all-Up distribution has H(A)=H(B)=0 → MI=0 by definition,
        // so we need at least two distinct states.
        let jc = counts_from(vec![
            ((MoveState::Up,   MoveState::Up),   5),
            ((MoveState::Down, MoveState::Down),  5),
        ]);
        let mi = normalized_mi(&jc, 5);
        assert!(mi > 0.90, "expected normalised MI ≈ 1.0 for perfectly co-moving markets, got {mi}");
    }

    #[test]
    fn mi_independent_gives_zero() {
        // Truly uniform 3×3 joint distribution → markets are independent → MI = 0.
        // All 9 cells equal weight means knowing one market's state gives zero
        // information about the other's state.
        let jc = counts_from(vec![
            ((MoveState::Up,   MoveState::Up),   10),
            ((MoveState::Up,   MoveState::Down), 10),
            ((MoveState::Up,   MoveState::Flat), 10),
            ((MoveState::Down, MoveState::Up),   10),
            ((MoveState::Down, MoveState::Down), 10),
            ((MoveState::Down, MoveState::Flat), 10),
            ((MoveState::Flat, MoveState::Up),   10),
            ((MoveState::Flat, MoveState::Down), 10),
            ((MoveState::Flat, MoveState::Flat), 10),
        ]);
        let mi = normalized_mi(&jc, 5);
        assert!(mi < 1e-9, "uniform joint distribution → MI ≈ 0, got {mi}");
    }

    #[test]
    fn mi_insufficient_data_returns_zero() {
        let jc = counts_from(vec![((MoveState::Up, MoveState::Up), 3)]);
        assert_eq!(normalized_mi(&jc, 10), 0.0);
    }

    // ── 4. Conditional probability ────────────────────────────────────────────

    #[test]
    fn conditional_directed_b_to_a() {
        // Whenever B goes up, A goes up (6 times). Otherwise random.
        let jc = counts_from(vec![
            ((MoveState::Up,   MoveState::Up),   6),  // A_up | B_up
            ((MoveState::Down, MoveState::Down),  6), // A_dn | B_dn
            ((MoveState::Up,   MoveState::Down),  0),
            ((MoveState::Down, MoveState::Up),    0),
        ]);
        let (p_up, p_dn) = directional_conditionals(&jc, 5).unwrap();
        assert!((p_up - 1.0).abs() < 1e-9, "P(A_up|B_up) should be 1.0, got {p_up}");
        assert!((p_dn - 1.0).abs() < 1e-9, "P(A_dn|B_dn) should be 1.0, got {p_dn}");
    }

    #[test]
    fn conditional_reverse_directed_a_to_b() {
        // Whenever A goes up, B goes up. Check reverse direction.
        let jc = counts_from(vec![
            ((MoveState::Up,   MoveState::Up),   6),
            ((MoveState::Down, MoveState::Down),  6),
        ]);
        let (p_up, p_dn) = directional_conditionals_reverse(&jc, 5).unwrap();
        assert!((p_up - 1.0).abs() < 1e-9, "P(B_up|A_up) should be 1.0, got {p_up}");
        assert!((p_dn - 1.0).abs() < 1e-9, "P(B_dn|A_dn) should be 1.0, got {p_dn}");
    }

    // ── 5. Embedding similarity ───────────────────────────────────────────────

    #[test]
    fn embedding_identical_ids_similarity_one() {
        let tokens = tokenize("trump-wins-election-2024");
        let emb = build_embedding(&tokens);
        let sim = cosine_similarity(&emb, &emb);
        assert!((sim - 1.0).abs() < 1e-9, "identical embeddings → sim=1.0, got {sim}");
    }

    #[test]
    fn embedding_disjoint_tokens_similarity_zero() {
        let ea = build_embedding(&tokenize("bitcoin-price-high"));
        let eb = build_embedding(&tokenize("election-winner-president"));
        let sim = cosine_similarity(&ea, &eb);
        assert_eq!(sim, 0.0, "disjoint tokens → sim=0.0, got {sim}");
    }

    #[test]
    fn embedding_partial_overlap() {
        let ea = build_embedding(&tokenize("trump-wins-election"));
        let eb = build_embedding(&tokenize("trump-loses-election"));
        let sim = cosine_similarity(&ea, &eb);
        // Shares "trump" and "election" but not "wins"/"loses".
        assert!(sim > 0.0 && sim < 1.0, "partial overlap → sim ∈ (0,1), got {sim}");
    }

    // ── 6. Composite scoring ──────────────────────────────────────────────────

    #[test]
    fn try_relationship_strong_correlation_emits() {
        let pair = ("market_a".to_string(), "market_b".to_string());
        // 20 paired samples with sinusoidal movements — returns are non-constant
        // so Pearson/Spearman on first-differences gives a meaningful correlation.
        let samples: Vec<(f64, f64)> = (0..20).map(|i| {
            let v = 0.50 + 0.20 * (i as f64 * std::f64::consts::PI / 5.0).sin();
            (v, v + 0.05)
        }).collect();
        let mut joint = joint_from(samples, 0.005);
        let emb_a = build_embedding(&tokenize("market_a"));
        let emb_b = build_embedding(&tokenize("market_b"));
        let cfg = test_config();
        let now = chrono::Utc::now();

        let result = try_relationship(&pair, &mut joint, &cfg, &emb_a, &emb_b, now);
        assert!(result.is_some(), "strongly correlated pair should produce a relationship");
        let rel = result.unwrap();
        assert_eq!(rel.market_a, "market_a");
        assert_eq!(rel.market_b, "market_b");
        assert!(rel.strength >= cfg.min_relationship_score);
        assert!(rel.correlation > 0.0);
    }

    #[test]
    fn try_relationship_insufficient_data_returns_none() {
        let pair = ("a".to_string(), "b".to_string());
        let mut joint = joint_from(vec![(0.5, 0.5), (0.6, 0.6)], 0.01); // only 2 pairs
        let cfg = test_config(); // min_history_len = 5
        let result = try_relationship(&pair, &mut joint, &cfg, &[], &[], chrono::Utc::now());
        assert!(result.is_none(), "too few observations should suppress emission");
    }

    #[test]
    fn try_relationship_deduplication_ttl() {
        let pair = ("a".to_string(), "b".to_string());
        // Sinusoidal series so returns have non-zero variance and Pearson fires.
        let samples: Vec<(f64, f64)> = (0..20).map(|i| {
            let v = 0.50 + 0.20 * (i as f64 * std::f64::consts::PI / 5.0).sin();
            (v, v)
        }).collect();
        let mut joint = joint_from(samples, 0.005);
        let cfg = RelationshipDiscoveryConfig {
            edge_ttl_secs: 3600,
            min_history_len: 5,
            min_relationship_score: 0.35,
            ..Default::default()
        };
        let now = chrono::Utc::now();
        let emb = build_embedding(&tokenize("market"));

        // First call: should emit.
        let first = try_relationship(&pair, &mut joint, &cfg, &emb, &emb, now);
        assert!(first.is_some(), "first call should emit");

        // Second call immediately: TTL not expired → should NOT emit.
        let second = try_relationship(&pair, &mut joint, &cfg, &emb, &emb, now);
        assert!(second.is_none(), "second call within TTL should be suppressed");
    }

    #[test]
    fn try_relationship_high_threshold_suppresses() {
        // A pair with perfect positive correlation has strength ≈ 0.4 (correlation only,
        // MI and embedding are 0 with no joint counts or shared tokens).
        // A threshold of 0.95 must suppress it.
        let pairs: Vec<(f64, f64)> = (0..20)
            .map(|i| {
                let v = 0.30 + 0.02 * i as f64;
                (v, v + 0.05)
            })
            .collect();
        let mut joint = joint_from(pairs, 0.005);
        let pair = ("zzz_a".to_string(), "zzz_b".to_string()); // no token overlap
        let result = try_relationship(
            &pair,
            &mut joint,
            &RelationshipDiscoveryConfig {
                min_relationship_score: 0.95,
                min_history_len: 5,
                edge_ttl_secs: 0,
                ..Default::default()
            },
            &[], // no embeddings → embedding_similarity = 0
            &[],
            chrono::Utc::now(),
        );
        assert!(result.is_none(), "score below 0.95 threshold should be suppressed");
    }

    // ── 7. Edge direction inference ───────────────────────────────────────────

    #[test]
    fn infer_direction_undirected_when_symmetric() {
        // Perfectly symmetric co-movement: both conditionals are 1.0 in both
        // directions → (a_follows_b=true, b_follows_a=true) → Undirected.
        let jc = counts_from(vec![
            ((MoveState::Up,   MoveState::Up),   10),
            ((MoveState::Down, MoveState::Down), 10),
        ]);
        let cfg = RelationshipDiscoveryConfig {
            directed_threshold: 0.65,
            min_history_len: 5,
            ..Default::default()
        };
        assert_eq!(infer_direction(&jc, &cfg), EdgeDirection::Undirected);
    }

    #[test]
    fn infer_direction_b_to_a_when_b_leads() {
        // B always causes A to move (a_follows_b = true).
        // A does NOT reliably cause B to move (b_follows_a = false).
        //
        // Counts constructed so that:
        //   B_up marginal  = 10  → P(A_up|B_up)  = 10/10 = 1.00 ≥ 0.65 ✓
        //   B_dn marginal  = 10  → P(A_dn|B_dn)  = 10/10 = 1.00 ≥ 0.65 ✓
        //   → a_follows_b = true
        //
        //   A_up marginal  = 10+12 = 22 → P(B_up|A_up) = 10/22 ≈ 0.45 < 0.65 ✗
        //   → b_follows_a = false  (only one conditional needs to fail)
        //
        //   (true, false) → BtoA
        let jc = counts_from(vec![
            ((MoveState::Up,   MoveState::Up),   10), // A up when B up
            ((MoveState::Down, MoveState::Down), 10), // A dn when B dn
            ((MoveState::Up,   MoveState::Flat), 12), // A also moves up independently of B
        ]);
        let cfg = RelationshipDiscoveryConfig {
            directed_threshold: 0.65,
            min_history_len: 5,
            ..Default::default()
        };
        assert_eq!(
            infer_direction(&jc, &cfg),
            EdgeDirection::BtoA,
            "B drives A → BtoA"
        );
    }
}
