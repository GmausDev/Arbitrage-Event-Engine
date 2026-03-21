// crates/portfolio_engine/src/config.rs

use serde::{Deserialize, Serialize};

/// Runtime configuration for `PortfolioEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PortfolioConfig {
    /// Starting bankroll in USD.
    pub initial_bankroll: f64,

    /// Maximum fraction of bankroll allowed in a single open position (0.0–1.0).
    ///
    /// Positions that exceed this cap are trimmed by `apply_risk_adjustments`.
    /// Should match `RiskConfig::max_position_fraction` for consistency.
    pub max_position_fraction: f64,

    /// How long (in seconds) a position may be held before it is automatically
    /// closed at its last known market price.  0 disables TTL expiry.
    pub position_ttl_secs: u64,
}

impl Default for PortfolioConfig {
    fn default() -> Self {
        Self {
            initial_bankroll:      10_000.0,
            max_position_fraction: 0.05,
            position_ttl_secs:     3_600, // 1 hour
        }
    }
}

impl PortfolioConfig {
    /// Validate configuration invariants.  Returns `Err` with a description
    /// if any field is outside its valid range.
    pub fn validate(&self) -> Result<(), String> {
        if self.initial_bankroll <= 0.0 {
            return Err(format!(
                "initial_bankroll must be positive, got {}",
                self.initial_bankroll
            ));
        }
        if !(0.0..=1.0).contains(&self.max_position_fraction) {
            return Err(format!(
                "max_position_fraction must be in [0.0, 1.0], got {}",
                self.max_position_fraction
            ));
        }
        Ok(())
    }

    /// Returns `true` if TTL expiry is enabled.
    pub fn ttl_enabled(&self) -> bool {
        self.position_ttl_secs > 0
    }
}
