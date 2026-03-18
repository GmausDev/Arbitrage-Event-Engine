// crates/performance_analytics/src/engine.rs

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::Utc;
use common::{Event, EventBus, MarketUpdate, Portfolio, PortfolioUpdate, Position, TradeDirection};
use metrics::{counter, gauge};
use tokio::sync::broadcast;
use tokio::time::{interval, Duration, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use crate::{
    config::AnalyticsConfig,
    edge_metrics::EdgeMetrics,
    types::{ExportFormat, PortfolioMetrics, RiskMetrics},
};

// ── PerformanceAnalytics ─────────────────────────────────────────────────────

/// Real-time performance tracker for the prediction market portfolio.
///
/// # Pure core
///
/// All metric computations (`build_metrics`, `compute_risk_metrics`,
/// `export_metrics`) are **pure functions** that operate on `&self` or
/// passed-in data.  The async `run()` loop only dispatches to these.
///
/// # Drawdown tracking
///
/// `peak_equity` is the global high-water mark across the full engine lifetime.
/// It is initialised to `NEG_INFINITY` so the first observed equity naturally
/// sets the baseline.  After history-window eviction `max_drawdown` may reflect
/// a trough that no longer appears in `get_historical_metrics()` — this is
/// intentional and documented.
pub struct PerformanceAnalytics {
    pub config: AnalyticsConfig,

    /// Rolling snapshot buffer, bounded to `config.history_window` entries.
    /// Index 0 is the oldest; the back is the most recent.
    pub history: VecDeque<PortfolioMetrics>,

    /// Latest known market probability per `market_id`, sourced from
    /// `Event::Market` updates.  Used to compute per-position MTM PnL.
    ///
    /// Pruned after every `process_portfolio_update` to retain only markets
    /// that currently have an open position, preventing unbounded growth.
    pub price_cache: HashMap<String, f64>,

    /// Global equity high-water mark.  Initialised to `NEG_INFINITY` so that
    /// the first observed equity becomes the baseline without bias.
    peak_equity: f64,

    /// Running maximum drawdown across the engine's full lifetime.
    max_drawdown: f64,

    /// Seven statistical edge diagnostics. Updated from the event loop
    /// alongside the portfolio/market tracking already present here.
    pub edge_metrics: EdgeMetrics,

    bus: EventBus,
}

impl PerformanceAnalytics {
    pub fn new(config: AnalyticsConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid AnalyticsConfig: {e}");
        }
        Self {
            config,
            history:      VecDeque::new(),
            price_cache:  HashMap::new(),
            // NEG_INFINITY ensures the first real equity observation sets the peak
            // rather than comparing against a config value that was never observed.
            peak_equity:  f64::NEG_INFINITY,
            max_drawdown: 0.0,
            edge_metrics: EdgeMetrics::new(),
            bus,
        }
    }

    // ── Pure core ─────────────────────────────────────────────────────────────

    /// Build a `PortfolioMetrics` snapshot from a portfolio update and the
    /// current price cache.
    ///
    /// Per-market PnL is computed for positions whose `market_id` is present
    /// in `price_cache`; absent entries are omitted rather than defaulted to
    /// zero so callers can distinguish "no data" from "break-even".
    ///
    /// The realized/unrealized split is an approximation: unrealized is the
    /// sum of MTM PnL for positions with a cached price; realized is the
    /// residual from `portfolio.pnl`.  A warning is emitted when some
    /// positions lack cached prices, as the split will be imprecise.
    pub fn build_metrics(
        portfolio:        &Portfolio,
        price_cache:      &HashMap<String, f64>,
        initial_bankroll: f64,
        timestamp:        chrono::DateTime<chrono::Utc>,
    ) -> PortfolioMetrics {
        // ── Per-position MTM ─────────────────────────────────────────────────
        let mut per_market_pnl = HashMap::new();
        let mut unrealized_pnl = 0.0_f64;
        let mut missing_prices = 0_usize;

        for pos in &portfolio.positions {
            if let Some(&current_price) = price_cache.get(&pos.market_id) {
                let mtm = Self::position_mtm(pos, current_price);
                per_market_pnl.insert(pos.market_id.clone(), mtm);
                unrealized_pnl += mtm;
            } else {
                missing_prices += 1;
            }
        }

        if missing_prices > 0 {
            warn!(
                missing = missing_prices,
                total   = portfolio.positions.len(),
                "build_metrics: positions without cached price \
                 — realized/unrealized split is approximate"
            );
        }

        // ── PnL split ────────────────────────────────────────────────────────
        // `portfolio.pnl` is total (realized + unrealized from PortfolioEngine).
        // We approximate realized as the residual after subtracting our MTM sum.
        // Accuracy improves as the price cache warms up.
        let total_pnl    = portfolio.pnl;
        let realized_pnl = total_pnl - unrealized_pnl;
        let equity       = initial_bankroll + total_pnl;

        // ── Exposure fraction ────────────────────────────────────────────────
        let exposure_fraction = if initial_bankroll > 0.0 {
            portfolio.exposure / initial_bankroll
        } else {
            0.0
        };

        PortfolioMetrics {
            total_pnl,
            realized_pnl,
            unrealized_pnl,
            exposure: portfolio.exposure,
            exposure_fraction,
            per_market_pnl,
            strategy_pnl: HashMap::new(), // v0.1: tags not on wire type
            position_count: portfolio.positions.len(),
            equity,
            timestamp,
        }
    }

    /// Mark-to-market PnL for a single position given a current market price.
    fn position_mtm(pos: &Position, current_price: f64) -> f64 {
        match pos.direction {
            TradeDirection::Buy | TradeDirection::Arbitrage => {
                (current_price - pos.entry_probability) * pos.size
            }
            TradeDirection::Sell => (pos.entry_probability - current_price) * pos.size,
        }
    }

    /// Ingest a `PortfolioUpdate` and push a new `PortfolioMetrics` snapshot
    /// onto the history buffer.
    ///
    /// Evicts the oldest entry when the buffer reaches `history_window`.
    /// Prunes `price_cache` to retain only markets with currently open
    /// positions, preventing unbounded cache growth.
    pub fn process_portfolio_update(&mut self, update: &PortfolioUpdate) {
        let metrics = Self::build_metrics(
            &update.portfolio,
            &self.price_cache,
            self.config.initial_bankroll,
            update.timestamp,
        );

        // ── Drawdown tracking ────────────────────────────────────────────────
        if metrics.equity > self.peak_equity {
            self.peak_equity = metrics.equity;
        }
        // Guard: only compute drawdown once we have a valid finite peak.
        let drawdown = if self.peak_equity.is_finite() && self.peak_equity > 0.0 {
            (self.peak_equity - metrics.equity) / self.peak_equity
        } else {
            0.0
        };
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }

        // ── Rolling buffer ───────────────────────────────────────────────────
        if self.history.len() >= self.config.history_window {
            self.history.pop_front();
        }
        self.history.push_back(metrics);

        // ── Prune price cache ────────────────────────────────────────────────
        // Keep only entries for markets that still have an open position.
        // This prevents the cache from growing indefinitely as markets resolve.
        let active: std::collections::HashSet<&str> = update
            .portfolio
            .positions
            .iter()
            .map(|p| p.market_id.as_str())
            .collect();
        self.price_cache.retain(|id, _| active.contains(id.as_str()));

        counter!("performance_analytics_snapshots_total").increment(1);
        trace!(
            history_len  = self.history.len(),
            total_pnl    = update.portfolio.pnl,
            peak_equity  = self.peak_equity,
            max_drawdown = self.max_drawdown,
            cache_size   = self.price_cache.len(),
            "performance_analytics: snapshot recorded"
        );
    }

    /// Update the internal price cache from a `MarketUpdate`.
    pub fn process_market_update(&mut self, update: &MarketUpdate) {
        self.price_cache
            .insert(update.market.id.clone(), update.market.probability);
    }

    /// Return the most recent snapshot, or `None` if no updates have been
    /// processed yet.
    pub fn compute_realtime_metrics(&self) -> Option<&PortfolioMetrics> {
        self.history.back()
    }

    /// Return a reference to the full history window, ordered oldest → newest.
    ///
    /// Prefer this over cloning when only iteration or inspection is needed.
    pub fn get_historical_metrics(&self) -> &VecDeque<PortfolioMetrics> {
        &self.history
    }

    /// Compute risk-adjusted metrics over the full history window.
    ///
    /// Returns a `RiskMetrics` with `sharpe_ratio = NAN` when fewer than
    /// 2 snapshots exist.
    pub fn compute_risk_metrics(&self) -> RiskMetrics {
        let now = Utc::now();

        if self.history.len() < 2 {
            return RiskMetrics {
                sharpe_ratio:     f64::NAN,
                max_drawdown:     self.max_drawdown,
                current_drawdown: 0.0,
                win_rate:         0.0,
                avg_tick_pnl:     0.0,
                volatility:       0.0,
                computed_at:      now,
            };
        }

        // ── Per-tick PnL returns ─────────────────────────────────────────────
        let returns: Vec<f64> = self
            .history
            .iter()
            .zip(self.history.iter().skip(1))
            .map(|(prev, curr)| curr.total_pnl - prev.total_pnl)
            .collect();

        let n        = returns.len() as f64;
        let mean     = returns.iter().sum::<f64>() / n;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / (n - 1.0).max(1.0);
        let volatility = variance.sqrt();

        // Sharpe = (mean / volatility) × annualisation_factor.
        // When volatility is zero, the Sharpe is infinite for positive mean
        // returns, negative-infinite for negative mean returns, and undefined
        // (NAN) for zero mean returns.
        let sharpe_ratio = if volatility > f64::EPSILON {
            (mean / volatility) * self.config.sharpe_ann_factor()
        } else if mean > f64::EPSILON {
            f64::INFINITY
        } else if mean < -f64::EPSILON {
            f64::NEG_INFINITY
        } else {
            f64::NAN // zero return, zero volatility — undefined
        };

        let wins     = returns.iter().filter(|&&r| r > 0.0).count();
        let win_rate = wins as f64 / n;

        // ── Current drawdown ─────────────────────────────────────────────────
        let current_equity   = self.history.back().map(|m| m.equity).unwrap_or(0.0);
        let current_drawdown = if self.peak_equity.is_finite() && self.peak_equity > 0.0 {
            ((self.peak_equity - current_equity) / self.peak_equity).max(0.0)
        } else {
            0.0
        };

        RiskMetrics {
            sharpe_ratio,
            max_drawdown: self.max_drawdown,
            current_drawdown,
            win_rate,
            avg_tick_pnl: mean,
            volatility,
            computed_at: now,
        }
    }

    /// Update the global Prometheus gauges so that scraping `:9000/metrics`
    /// includes portfolio performance (PnL, Sharpe, drawdown, etc.).
    ///
    /// Called automatically after each `Event::Portfolio` when history is non-empty.
    fn record_performance_gauges(&self, m: &PortfolioMetrics) {
        gauge!("portfolio_total_pnl").set(m.total_pnl);
        gauge!("portfolio_realized_pnl").set(m.realized_pnl);
        gauge!("portfolio_unrealized_pnl").set(m.unrealized_pnl);
        gauge!("portfolio_exposure").set(m.exposure);
        gauge!("portfolio_exposure_fraction").set(m.exposure_fraction);
        gauge!("portfolio_equity").set(m.equity);
        gauge!("portfolio_position_count").set(m.position_count as f64);

        let risk = self.compute_risk_metrics();
        let sharpe = match risk.sharpe_ratio {
            v if v.is_nan() => 0.0,
            v if v == f64::INFINITY => f64::MAX,
            v if v == f64::NEG_INFINITY => f64::MIN,
            v => v,
        };
        gauge!("portfolio_sharpe_ratio").set(sharpe);
        gauge!("portfolio_max_drawdown").set(risk.max_drawdown);
        gauge!("portfolio_win_rate").set(risk.win_rate);
    }

    /// Serialise the history window (or the latest snapshot for Prometheus)
    /// to a `String` in the requested format.
    ///
    /// Returns an empty string / empty JSON array when history is empty.
    pub fn export_metrics(&self, format: ExportFormat) -> String {
        match format {
            ExportFormat::Json        => self.export_json(),
            ExportFormat::Csv         => self.export_csv(),
            ExportFormat::Prometheus  => self.export_prometheus(),
        }
    }

    // ── Export helpers ─────────────────────────────────────────────────────

    fn export_json(&self) -> String {
        let history: Vec<&PortfolioMetrics> = self.history.iter().collect();
        match serde_json::to_string_pretty(&history) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = %e, "export_json: serialization failed");
                String::from("[]")
            }
        }
    }

    fn export_csv(&self) -> String {
        let mut out = String::from(
            "timestamp,total_pnl,realized_pnl,unrealized_pnl,\
             exposure,exposure_fraction,position_count,equity\n",
        );
        for m in &self.history {
            out.push_str(&format!(
                "{},{:.6},{:.6},{:.6},{:.6},{:.6},{},{:.6}\n",
                m.timestamp.to_rfc3339(),
                m.total_pnl,
                m.realized_pnl,
                m.unrealized_pnl,
                m.exposure,
                m.exposure_fraction,
                m.position_count,
                m.equity,
            ));
        }
        out
    }

    fn export_prometheus(&self) -> String {
        let Some(m) = self.history.back() else {
            return String::new();
        };
        let risk = self.compute_risk_metrics();
        let ts   = m.timestamp.timestamp_millis();

        // Guard non-finite Sharpe before emitting — Prometheus text format
        // requires a valid float.
        let sharpe = match risk.sharpe_ratio {
            v if v.is_nan()              => 0.0,
            v if v == f64::INFINITY      => f64::MAX,
            v if v == f64::NEG_INFINITY  => f64::MIN,
            v                            => v,
        };

        format!(
            "# HELP portfolio_total_pnl Total PnL in USD (realized + unrealized)\n\
             # TYPE portfolio_total_pnl gauge\n\
             portfolio_total_pnl {:.6} {ts}\n\
             # HELP portfolio_realized_pnl Realized PnL in USD\n\
             # TYPE portfolio_realized_pnl gauge\n\
             portfolio_realized_pnl {:.6} {ts}\n\
             # HELP portfolio_unrealized_pnl Unrealized PnL in USD\n\
             # TYPE portfolio_unrealized_pnl gauge\n\
             portfolio_unrealized_pnl {:.6} {ts}\n\
             # HELP portfolio_exposure Total deployed capital in USD\n\
             # TYPE portfolio_exposure gauge\n\
             portfolio_exposure {:.6} {ts}\n\
             # HELP portfolio_exposure_fraction Exposure as fraction of bankroll\n\
             # TYPE portfolio_exposure_fraction gauge\n\
             portfolio_exposure_fraction {:.6} {ts}\n\
             # HELP portfolio_equity Equity (bankroll + total PnL) in USD\n\
             # TYPE portfolio_equity gauge\n\
             portfolio_equity {:.6} {ts}\n\
             # HELP portfolio_position_count Number of open positions\n\
             # TYPE portfolio_position_count gauge\n\
             portfolio_position_count {} {ts}\n\
             # HELP portfolio_sharpe_ratio Annualised Sharpe ratio\n\
             # TYPE portfolio_sharpe_ratio gauge\n\
             portfolio_sharpe_ratio {:.6} {ts}\n\
             # HELP portfolio_max_drawdown Maximum peak-to-trough drawdown fraction\n\
             # TYPE portfolio_max_drawdown gauge\n\
             portfolio_max_drawdown {:.6} {ts}\n\
             # HELP portfolio_win_rate Fraction of ticks with positive PnL change\n\
             # TYPE portfolio_win_rate gauge\n\
             portfolio_win_rate {:.6} {ts}\n",
            m.total_pnl,
            m.realized_pnl,
            m.unrealized_pnl,
            m.exposure,
            m.exposure_fraction,
            m.equity,
            m.position_count,
            sharpe,
            risk.max_drawdown,
            risk.win_rate,
        )
    }

    // ── Event loop ────────────────────────────────────────────────────────────

    /// Subscribe to events on the bus and drive all performance and edge metrics.
    ///
    /// **Events handled:**
    /// - `Event::Portfolio` — portfolio snapshots (existing metrics)
    /// - `Event::Market`    — price updates (existing metrics + EdgeMetrics M3/M4/M7)
    /// - `Event::Signal`    — strategy signals (EdgeMetrics M1/M3/M4/M6/M7)
    /// - `Event::Execution` — trade fills (EdgeMetrics M2/M5)
    ///
    /// A 60-second tick drives EdgeMetrics TTL expiry (M4).
    ///
    /// Runs until `cancel` fires or the bus closes.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx: broadcast::Receiver<Arc<Event>> = self.bus.subscribe();

        // Periodic tick for EdgeMetrics TTL expiry (signal precision, M4).
        let mut edge_tick = interval(Duration::from_secs(60));
        edge_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // First tick fires immediately — skip it so we don't expire before anything arrives.
        edge_tick.tick().await;

        info!(
            history_window    = self.config.history_window,
            initial_bankroll  = self.config.initial_bankroll,
            tick_interval_sec = self.config.tick_interval_secs,
            "performance_analytics: started"
        );

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("performance_analytics: shutdown requested, exiting");
                    break;
                }

                _ = edge_tick.tick() => {
                    self.edge_metrics.tick();
                }

                result = rx.recv() => {
                    match result {
                        Ok(ev) => match ev.as_ref() {
                            Event::Signal(signal) => {
                                counter!("performance_analytics_signal_events_total").increment(1);
                                self.edge_metrics.on_signal(signal);
                            }

                            Event::Execution(exec) => {
                                counter!("performance_analytics_execution_events_total").increment(1);
                                // on_execution reads the price cache that was just updated by
                                // the most recent Market event, so ordering is correct.
                                self.edge_metrics.on_execution(exec);
                            }

                            Event::Portfolio(update) => {
                                counter!("performance_analytics_portfolio_events_total").increment(1);
                                self.process_portfolio_update(update);

                                if let Some(m) = self.compute_realtime_metrics() {
                                    self.record_performance_gauges(m);
                                    debug!(
                                        total_pnl     = m.total_pnl,
                                        equity        = m.equity,
                                        positions     = m.position_count,
                                        exposure_frac = m.exposure_fraction,
                                        "performance_analytics: metrics updated"
                                    );
                                }
                            }

                            Event::Market(update) => {
                                counter!("performance_analytics_market_events_total").increment(1);
                                self.process_market_update(update);
                                self.edge_metrics.on_market_update(update);
                            }

                            _ => {}
                        },

                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("performance_analytics: lagged by {n} events");
                        }

                        Err(broadcast::error::RecvError::Closed) => {
                            info!("performance_analytics: event bus closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }
    }
}
