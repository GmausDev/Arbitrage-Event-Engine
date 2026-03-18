// crates/world_model/src/constraints.rs
//
// Logical constraint checking for the World Model Engine.
//
// Each constraint type defines what the probabilities of a set of markets
// *should* satisfy.  If the actual values deviate beyond `tolerance`, a
// `ConstraintViolation` is returned for the caller to publish as
// `Event::Inconsistency`.
//
// Supported constraint types
// ──────────────────────────
// MutualExclusion / SumToOne
//   All listed markets are mutually exclusive outcomes.
//   Violation = |Σ P(m_i) − 1|
//
// And
//   The *last* market in `markets_involved` represents the conjunction of
//   all preceding markets.  P(A ∧ B) cannot exceed min(P(A), P(B)).
//   Violation = max(0, P(conjunction) − min_parent)
//
// Or
//   The *last* market represents the disjunction of preceding markets.
//   P(A ∨ B) must be ≥ max(P(A), P(B)).
//   Violation = max(0, max_input − P(disjunction))

use std::collections::HashMap;

use crate::types::{ConstraintType, ConstraintViolation, WorldState};

/// Check all registered constraints against the current world state.
///
/// Returns one [`ConstraintViolation`] per violated constraint.
/// Constraints whose markets have not yet been seeded into the world state
/// are silently skipped (no violation reported for missing data).
pub fn check_constraints(state: &WorldState) -> Vec<ConstraintViolation> {
    let mut violations = Vec::new();

    for constraint in &state.constraints {
        // Gather current probabilities; skip if any market is missing.
        let current_probs: HashMap<String, f64> = constraint
            .markets_involved
            .iter()
            .filter_map(|id| {
                state
                    .variables
                    .get(id)
                    .map(|v| (id.clone(), v.current_probability))
            })
            .collect();

        if current_probs.len() < constraint.markets_involved.len() {
            // Not all markets are in the world state yet — skip.
            continue;
        }

        let probs: Vec<f64> = constraint
            .markets_involved
            .iter()
            .map(|id| *current_probs.get(id).unwrap_or(&0.0))
            .collect();

        let magnitude: f64 = match constraint.constraint_type {
            ConstraintType::MutualExclusion | ConstraintType::SumToOne => {
                let sum: f64 = probs.iter().sum();
                (sum - 1.0).abs()
            }

            ConstraintType::And => {
                // Need at least one parent and one conjunction market.
                if probs.len() < 2 {
                    continue;
                }
                let min_parent = probs[..probs.len() - 1]
                    .iter()
                    .cloned()
                    .fold(f64::INFINITY, f64::min);
                let conjunction = probs[probs.len() - 1];
                // Violation if the conjunction probability exceeds min(parents).
                (conjunction - min_parent).max(0.0)
            }

            ConstraintType::Or => {
                // Need at least one input and one disjunction market.
                if probs.len() < 2 {
                    continue;
                }
                let max_input = probs[..probs.len() - 1]
                    .iter()
                    .cloned()
                    .fold(f64::NEG_INFINITY, f64::max);
                let disjunction = probs[probs.len() - 1];
                // Violation if the disjunction probability is below max(inputs).
                (max_input - disjunction).max(0.0)
            }
        };

        if magnitude > constraint.tolerance {
            violations.push(ConstraintViolation {
                constraint_id: constraint.constraint_id.clone(),
                constraint_type: constraint.constraint_type, // Copy
                markets_involved: constraint.markets_involved.clone(),
                current_probabilities: current_probs,
                violation_magnitude: magnitude,
            });
        }
    }

    violations
}
