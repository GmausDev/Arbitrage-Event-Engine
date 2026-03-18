// crates/performance_analytics/src/config.rs

use serde::{Deserialize, Serialize};

/// Runtime configuration for `PerformanceAnalytics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalyticsConfig {
    /// Maximum number of `PortfolioMetrics` snapshots to retain in the rolling
    /// history buffer.  Oldest entries are evicted when the buffer is full.
    pub history_window: usize,

    /// Starting bankroll in USD.  Used as the denominator for return
    /// calculations and exposure-fraction reporting.
    ///
    /// Should match `PortfolioConfig::initial_bankroll`.
    pub initial_bankroll: f64,

    /// Interval between portfolio snapshots in seconds.
    ///
    /// Used to derive the Sharpe annualisation factor:
    /// `sqrt(seconds_per_year / tick_interval_secs)`.
    ///
    /// Default: `1.0` — one-second ticks (matches the system default from CLAUDE.md).
    /// Set to `60.0` for minute-level ticks, `3600.0` for hourly, etc.
    pub tick_interval_secs: f64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            history_window:    1_000,
            initial_bankroll:  10_000.0,
            tick_interval_secs: 60.0, // one-minute snapshots → ann_factor ≈ 725
        }
    }
}

impl AnalyticsConfig {
    /// Sharpe annualisation factor derived from the configured tick interval.
    ///
    /// `sqrt(seconds_per_year / tick_interval_secs)`.
    pub fn sharpe_ann_factor(&self) -> f64 {
        const SECONDS_PER_YEAR: f64 = 365.25 * 24.0 * 3600.0;
        (SECONDS_PER_YEAR / self.tick_interval_secs).sqrt()
    }

    /// Validate configuration invariants.  Returns `Err` with a description
    /// if any field is outside its valid range.
    pub fn validate(&self) -> Result<(), String> {
        if self.history_window == 0 {
            return Err("history_window must be at least 1".to_string());
        }
        if !self.initial_bankroll.is_finite() || self.initial_bankroll <= 0.0 {
            return Err(format!(
                "initial_bankroll must be finite and positive, got {}",
                self.initial_bankroll
            ));
        }
        if !self.tick_interval_secs.is_finite() || self.tick_interval_secs <= 0.0 {
            return Err(format!(
                "tick_interval_secs must be finite and positive, got {}",
                self.tick_interval_secs
            ));
        }
        Ok(())
    }
}
