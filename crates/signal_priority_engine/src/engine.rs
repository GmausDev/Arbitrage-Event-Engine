// crates/signal_priority_engine/src/engine.rs

use std::sync::Arc;

use chrono::Utc;
use common::{
    Event, EventBus, FastSignal, PrioritizedSignal, SignalSpeed, TopSignalsBatch, TradeSignal,
};
use metrics::counter;
use tokio::sync::broadcast;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::PriorityConfig;

// ── Internal ranked entry ──────────────────────────────────────────────────────

/// A signal held in the slow-path buffer, ordered by priority score.
struct Queued {
    signal:         TradeSignal,
    priority_score: f64,
}

// ── SignalPriorityEngine ───────────────────────────────────────────────────────

/// Routes incoming `Event::Signal` events to the fast or slow execution path.
///
/// # Fast path (`expected_value × confidence ≥ fast_raev_threshold`)
///
/// The signal is published immediately as `Event::FastSignal`.  `RiskEngine`
/// consumes `FastSignal` directly, bypassing the `PortfolioOptimizer` batch
/// window.  This minimises latency for the highest-edge opportunities.
///
/// # Slow path
///
/// Signals below the threshold are buffered in a priority queue (ranked by
/// `expected_value × confidence`, descending).  Every `batch_window_ms`
/// milliseconds the top `top_n` signals are emitted as a single
/// `Event::TopSignalsBatch` consumed by `PortfolioOptimizer`.
///
/// # Pipeline position
///
/// ```text
/// strategy agents  ──→  Event::Signal
///                           └─→ SignalPriorityEngine   ← this module
///                                   ├─→ Event::FastSignal   → RiskEngine
///                                   └─→ Event::TopSignalsBatch → PortfolioOptimizer
/// ```
pub struct SignalPriorityEngine {
    config: PriorityConfig,
    bus:    EventBus,
    rx:     Option<broadcast::Receiver<Arc<Event>>>,
    /// Buffered slow-path signals; flushed every `batch_window_ms`.
    queue:  Vec<Queued>,
}

impl SignalPriorityEngine {
    pub fn new(config: PriorityConfig, bus: EventBus) -> Result<Self, anyhow::Error> {
        config
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid PriorityConfig: {e}"))?;
        let rx = bus.subscribe();
        Ok(Self {
            config,
            bus,
            rx: Some(rx),
            queue: Vec::new(),
        })
    }

    // ── Classification ─────────────────────────────────────────────────────────

    /// Compute the ranking score and speed class for a signal.
    ///
    /// Score = `expected_value × confidence`.
    /// Fast if score ≥ `config.fast_raev_threshold` and EV > 0.
    fn classify(&self, signal: &TradeSignal) -> (f64, SignalSpeed) {
        let score = signal.expected_value * signal.confidence;
        // Require strictly positive EV and non-negative confidence so that a
        // large-negative × large-negative product cannot spuriously exceed the
        // fast threshold and route a money-losing signal to the fast path.
        let speed = if score.is_finite()
            && score >= self.config.fast_raev_threshold
            && signal.expected_value > 0.0
            && signal.confidence >= 0.0
        {
            SignalSpeed::Fast
        } else {
            SignalSpeed::Slow
        };
        (score, speed)
    }

    // ── Fast path ─────────────────────────────────────────────────────────────

    fn emit_fast(&self, signal: TradeSignal, priority_score: f64) {
        counter!("priority_engine_fast_signals_total").increment(1);
        debug!(
            market_id      = %signal.market_id,
            priority_score,
            "priority_engine: fast signal → immediate execution path"
        );
        let ev = Event::FastSignal(FastSignal { signal, priority_score });
        if let Err(e) = self.bus.publish(ev) {
            warn!("priority_engine: failed to publish FastSignal: {e}");
        }
    }

    // ── Slow path ─────────────────────────────────────────────────────────────

    fn buffer_slow(&mut self, signal: TradeSignal, priority_score: f64) {
        // Reject NaN/Inf scores — they poison sort ordering.
        if !priority_score.is_finite() {
            counter!("priority_engine_signals_dropped_total").increment(1);
            return;
        }
        // Pre-filter: discard below min_slow_ev.
        if signal.expected_value < self.config.min_slow_ev {
            counter!("priority_engine_signals_dropped_total").increment(1);
            return;
        }
        counter!("priority_engine_slow_signals_buffered_total").increment(1);
        self.queue.push(Queued { signal, priority_score });

        // Evict lowest-scoring entries if over capacity.
        if self.queue.len() > self.config.slow_queue_capacity {
            // Sort descending; keep top `slow_queue_capacity`.
            self.queue.sort_unstable_by(|a, b| {
                b.priority_score
                    .partial_cmp(&a.priority_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let evicted = self.queue.len() - self.config.slow_queue_capacity;
            self.queue.truncate(self.config.slow_queue_capacity);
            counter!("priority_engine_signals_evicted_total").increment(evicted as u64);
        }
    }

    // ── Flush ─────────────────────────────────────────────────────────────────

    /// Sort the slow queue, take the top `top_n`, emit as `TopSignalsBatch`.
    fn flush(&mut self) {
        if self.queue.is_empty() {
            return;
        }

        // Sort descending by priority score.
        self.queue.sort_unstable_by(|a, b| {
            b.priority_score
                .partial_cmp(&a.priority_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let take_n = self.queue.len().min(self.config.top_n);
        let drained: Vec<Queued> = self.queue.drain(..take_n).collect();

        // Drop the remainder (lower-priority signals that didn't make the top-N
        // cut this flush window).  Use a distinct metric name from the capacity-
        // eviction counter in `buffer_slow` to avoid operator confusion.
        let remainder = self.queue.len();
        if remainder > 0 {
            counter!("priority_engine_batch_remainder_dropped_total").increment(remainder as u64);
        }
        self.queue.clear();

        let signals: Vec<PrioritizedSignal> = drained
            .into_iter()
            .map(|q| PrioritizedSignal {
                signal:         q.signal,
                priority_score: q.priority_score,
                speed:          SignalSpeed::Slow,
            })
            .collect();

        let n = signals.len() as u64;
        counter!("priority_engine_batch_signals_emitted_total").increment(n);

        debug!(
            count = n,
            "priority_engine: flushing TopSignalsBatch"
        );

        let ev = Event::TopSignalsBatch(TopSignalsBatch {
            signals,
            window_ms:  self.config.batch_window_ms,
            timestamp:  Utc::now(),
        });
        if let Err(e) = self.bus.publish(ev) {
            warn!("priority_engine: failed to publish TopSignalsBatch: {e}");
        }
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx   = self.rx.take().expect("rx already consumed");
        let mut tick = time::interval(Duration::from_millis(self.config.batch_window_ms));
        tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        info!(
            batch_window_ms      = self.config.batch_window_ms,
            top_n                = self.config.top_n,
            fast_raev_threshold  = self.config.fast_raev_threshold,
            "priority_engine: started"
        );

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("priority_engine: shutdown — flushing final batch");
                    self.flush();
                    break;
                }

                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            if let Event::Signal(signal) = ev.as_ref() {
                                counter!("priority_engine_signals_seen_total").increment(1);
                                let (score, speed) = self.classify(signal);
                                match speed {
                                    SignalSpeed::Fast => self.emit_fast(signal.clone(), score),
                                    SignalSpeed::Slow => self.buffer_slow(signal.clone(), score),
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("priority_engine: lagged by {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("priority_engine: bus closed, shutting down");
                            break;
                        }
                    }
                }

                _ = tick.tick() => {
                    self.flush();
                }
            }
        }
    }
}
