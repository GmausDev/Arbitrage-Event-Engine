// crates/risk_engine/src/types.rs

/// Reason a `TradeSignal` was rejected by the Risk Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    /// `expected_value < min_expected_value`
    LowExpectedValue,
    /// `(peak_equity − current_equity) / peak_equity > max_drawdown`
    DrawdownProtection,
    /// `total_exposure >= max_total_exposure` — no room for any size
    TotalExposureFull,
    /// Cluster's exposure is already at or above `max_cluster_exposure`
    ClusterExposureFull,
    /// Approved size collapsed to ≤ 0 after all resize steps
    ZeroApprovedSize,
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LowExpectedValue    => write!(f, "expected value below minimum"),
            Self::DrawdownProtection  => write!(f, "drawdown protection triggered"),
            Self::TotalExposureFull   => write!(f, "total exposure limit reached"),
            Self::ClusterExposureFull => write!(f, "cluster exposure limit reached"),
            Self::ZeroApprovedSize    => write!(f, "approved size collapsed to zero"),
        }
    }
}
