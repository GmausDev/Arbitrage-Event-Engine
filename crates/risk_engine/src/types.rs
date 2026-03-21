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
    /// `net_edge = gross_edge − total_cost ≤ 0`: costs exceed the raw edge
    NegativeNetEdge,
    /// `expected_profit = net_edge × position_size_usd < min_expected_profit_usd`
    InsufficientExpectedProfit,
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LowExpectedValue    => write!(f, "expected value below minimum"),
            Self::DrawdownProtection  => write!(f, "drawdown protection triggered"),
            Self::TotalExposureFull   => write!(f, "total exposure limit reached"),
            Self::ClusterExposureFull => write!(f, "cluster exposure limit reached"),
            Self::ZeroApprovedSize             => write!(f, "approved size collapsed to zero"),
            Self::NegativeNetEdge              => write!(f, "net edge negative after costs"),
            Self::InsufficientExpectedProfit   => write!(f, "expected profit below minimum threshold"),
        }
    }
}
