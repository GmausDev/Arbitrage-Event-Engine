// agents/arb_agent/src/lib.rs
// Detects pricing inefficiencies between model probabilities and market prices,
// and between correlated markets, then emits TradeSignal events.
//
// Subscribes to: Event::Market(MarketUpdate), Event::Graph(GraphUpdate)
// Publishes:     Event::Signal(TradeSignal)

use chrono::Utc;
use common::{Event, EventBus, MarketNode, TradeDirection, TradeSignal};
use std::collections::HashMap;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Minimum model vs. market probability gap to trigger a signal.
/// Configurable via TOML config (see Configuration & Monitoring).
const ARB_THRESHOLD: f64 = 0.05;

pub struct ArbAgent {
    /// Latest market-reported probabilities
    market_probs: HashMap<String, MarketNode>,
    /// TODO: receive model probabilities from bayesian_engine via bus
    bus: EventBus,
}

impl ArbAgent {
    pub fn new(bus: EventBus) -> Self {
        Self {
            market_probs: HashMap::new(),
            bus,
        }
    }

    /// Check whether the gap between model and market probability exceeds
    /// the arbitrage threshold, and emit a TradeSignal if so.
    ///
    /// model_prob would come from BayesianEngine output (future: dedicated event).
    fn check_arb(&self, market: &MarketNode, model_prob: f64) -> Option<TradeSignal> {
        let gap = model_prob - market.probability;

        if gap.abs() < ARB_THRESHOLD {
            return None;
        }

        let direction = if gap > 0.0 {
            TradeDirection::Buy  // model says higher than market → buy YES
        } else {
            TradeDirection::Sell // model says lower than market → sell YES (buy NO)
        };

        // NOTE: arb_agent is a legacy stub; signal_agent now owns signal generation
        // with proper EV and Kelly sizing. Fields below use conservative placeholders.
        Some(TradeSignal {
            market_id: market.id.clone(),
            direction,
            expected_value: gap.abs(),
            position_fraction: 0.0,
            posterior_prob: model_prob,
            market_prob: market.probability,
            confidence: 0.0,
            timestamp: Utc::now(),
            source: "arb_agent".to_string(),
        })
    }

    pub async fn run(mut self) {
        let mut rx = self.bus.subscribe();
        info!("arb_agent: started, threshold = {ARB_THRESHOLD}");

        loop {
            match rx.recv().await {
                Ok(ev) => match ev.as_ref() {
                    Event::Market(update) => {
                        let market = update.market.clone();
                        self.market_probs
                            .insert(market.id.clone(), market.clone());

                        // TODO: retrieve model probability from BayesianEngine state.
                        //       For now, use market probability as a stand-in (no signal generated).
                        let model_prob = market.probability; // placeholder

                        if let Some(signal) = self.check_arb(&market, model_prob) {
                            info!(
                                market_id = %signal.market_id,
                                direction = ?signal.direction,
                                expected_value = signal.expected_value,
                                "arb_agent: emitting TradeSignal"
                            );
                            if let Err(e) = self.bus.publish(Event::Signal(signal)) {
                                warn!("arb_agent: failed to publish TradeSignal: {e}");
                            }
                        }
                    }
                    Event::Graph(_graph_update) => {
                        // TODO: scan graph edges for cross-market arb opportunities
                    }
                    _ => {}
                },
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("arb_agent: lagged by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("arb_agent: bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
