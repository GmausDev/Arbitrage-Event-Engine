// crates/calibration/src/engine.rs

use std::sync::Arc;

use chrono::Utc;
use common::{CalibrationUpdate, Event, EventBus};
use metrics::counter;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{config::CalibrationConfig, state::CalibrationState};

pub struct CalibrationEngine {
    state: Arc<RwLock<CalibrationState>>,
    bus:   EventBus,
}

impl CalibrationEngine {
    pub fn new(config: CalibrationConfig, bus: EventBus) -> Self {
        Self {
            state: Arc::new(RwLock::new(CalibrationState::new(config))),
            bus,
        }
    }

    /// Run until `cancel` fires or the bus closes.
    ///
    /// Subscribes to:
    /// - `Event::Posterior` — records the model's prediction for a market
    /// - `Event::Market`    — detects market resolution and scores predictions
    pub async fn run(self, cancel: CancellationToken) {
        let mut rx: broadcast::Receiver<Arc<Event>> = self.bus.subscribe();
        info!("calibration: started — tracking posterior predictions");

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("calibration: shutdown requested, exiting");
                    break;
                }
                result = rx.recv() => result,
            };

            match event {
                Ok(ev) => match ev.as_ref() {
                    Event::Posterior(p) => {
                        let mut state = self.state.write().await;
                        state.on_posterior(&p.market_id, p.posterior_prob, p.timestamp);
                    }

                    Event::Market(m) => {
                        let result = {
                            let mut state = self.state.write().await;
                            state.on_market_update(&m.market.id, m.market.probability)
                        };

                        if let Some(record) = result {
                            counter!("calibration_resolutions_scored_total").increment(1);

                            let (overall_brier, calibration_error) = {
                                let state = self.state.read().await;
                                (state.overall_brier(), state.calibration_error())
                            };

                            debug!(
                                market_id       = %record.market_id,
                                predicted       = record.predicted_prob,
                                outcome         = record.outcome,
                                brier           = record.brier_score,
                                overall_brier,
                                calibration_error,
                                "calibration: market resolved — scored prediction"
                            );

                            let update = CalibrationUpdate {
                                market_id:         record.market_id,
                                predicted_prob:    record.predicted_prob,
                                outcome:           record.outcome,
                                brier_score:       record.brier_score,
                                overall_brier,
                                calibration_error,
                                timestamp:         Utc::now(),
                            };

                            if let Err(e) = self.bus.publish(Event::CalibrationUpdate(update)) {
                                warn!("calibration: failed to publish CalibrationUpdate: {e}");
                            }
                        }
                    }

                    _ => {}
                },

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("calibration: lagged by {n} events");
                }

                Err(broadcast::error::RecvError::Closed) => {
                    info!("calibration: event bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
