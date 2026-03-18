// crates/data_ingestion/src/scheduler/polling_manager.rs
//
// Runs a set of connector polling tasks concurrently.
//
// Each `PollerTask` owns:
//   • A `ConnectorRunner` — async fn that fetches, normalises, caches, and publishes.
//   • An `interval` — how often to fire.
//   • A `RateLimiter` — token-bucket guard.
//
// The manager spawns one tokio task per registered connector.  Tasks shut down
// gracefully when the `CancellationToken` fires.

use std::sync::Arc;

use async_trait::async_trait;
use metrics::counter;
use tokio::{
    sync::RwLock,
    time::{interval, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::cache::snapshot_cache::SnapshotCache;
use crate::scheduler::rate_limiter::RateLimiter;
use common::EventBus;

// ---------------------------------------------------------------------------
// ConnectorRunner trait
// ---------------------------------------------------------------------------

/// Abstraction over one connector's poll-normalise-cache-publish cycle.
#[async_trait]
pub trait ConnectorRunner: Send + Sync {
    async fn run_once(
        &self,
        bus: EventBus,
        cache: Arc<RwLock<SnapshotCache>>,
    ) -> anyhow::Result<usize>;
}

// ---------------------------------------------------------------------------
// PollerTask
// ---------------------------------------------------------------------------

pub struct PollerTask {
    pub name: String,
    pub poll_interval: Duration,
    pub rate_limiter: RateLimiter,
    pub runner: Box<dyn ConnectorRunner>,
}

// ---------------------------------------------------------------------------
// PollingManager
// ---------------------------------------------------------------------------

pub struct PollingManager {
    tasks: Vec<PollerTask>,
    bus: EventBus,
    cache: Arc<RwLock<SnapshotCache>>,
}

impl PollingManager {
    pub fn new(bus: EventBus, cache: Arc<RwLock<SnapshotCache>>) -> Self {
        Self {
            tasks: vec![],
            bus,
            cache,
        }
    }

    pub fn add_task(&mut self, task: PollerTask) {
        self.tasks.push(task);
    }

    /// Spawn one tokio task per registered connector and drive them until
    /// `cancel` fires.
    pub async fn run(self, cancel: CancellationToken) {
        let mut handles = Vec::with_capacity(self.tasks.len());
        for task in self.tasks {
            let bus = self.bus.clone();
            let cache = self.cache.clone();
            let cancel = cancel.clone();
            handles.push(tokio::spawn(run_poller_loop(task, bus, cache, cancel)));
        }
        for h in handles {
            let _ = h.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Per-task polling loop
// ---------------------------------------------------------------------------

async fn run_poller_loop(
    mut task: PollerTask,
    bus: EventBus,
    cache: Arc<RwLock<SnapshotCache>>,
    cancel: CancellationToken,
) {
    let name = task.name.clone();
    let mut ticker = interval(task.poll_interval);
    // First tick fires immediately, starting the first poll at launch.

    info!(connector = name, "polling_manager: task started");

    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                info!(connector = name, "polling_manager: task shutting down");
                break;
            }

            _ = ticker.tick() => {
                // Rate-limit: wait for a token before hitting the API.
                // The acquire() is itself wrapped in a select! so that a
                // cancellation firing while we're waiting for a token (which
                // can take up to 1/rps seconds) is handled immediately.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        info!(connector = name, "polling_manager: task shutting down during rate-limit wait");
                        break;
                    }
                    _ = task.rate_limiter.acquire() => {}
                }

                // Run the connector but abort immediately if cancel fires.
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        info!(connector = name, "polling_manager: task shutting down mid-poll");
                        break;
                    }
                    r = task.runner.run_once(bus.clone(), cache.clone()) => r,
                };

                match result {
                    Ok(n) => {
                        debug!(connector = name, events = n, "polling_manager: poll complete");
                        counter!("data_ingestion_poll_success_total").increment(1);
                    }
                    Err(e) => {
                        warn!(connector = name, err = %e, "polling_manager: poll error");
                        counter!("data_ingestion_poll_error_total").increment(1);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    struct CountingRunner(StdArc<AtomicUsize>);

    #[async_trait]
    impl ConnectorRunner for CountingRunner {
        async fn run_once(
            &self,
            _bus: EventBus,
            _cache: Arc<RwLock<SnapshotCache>>,
        ) -> anyhow::Result<usize> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        }
    }

    #[tokio::test]
    async fn polling_manager_runs_task_and_cancels() {
        let bus = common::EventBus::new();
        let cache = Arc::new(RwLock::new(SnapshotCache::new(10, 10)));

        let counter = StdArc::new(AtomicUsize::new(0));

        let task = PollerTask {
            name: "test".into(),
            poll_interval: Duration::from_millis(20),
            rate_limiter: RateLimiter::new(100.0, 10.0),
            runner: Box::new(CountingRunner(counter.clone())),
        };

        let cancel = CancellationToken::new();
        let mut mgr = PollingManager::new(bus, cache);
        mgr.add_task(task);

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            mgr.run(cancel_clone).await;
        });

        // Let it run for ~100 ms then cancel
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Should have polled at least a few times
        assert!(counter.load(Ordering::Relaxed) >= 2, "expected at least 2 polls");
    }

    #[tokio::test]
    async fn polling_manager_handles_runner_error() {
        struct ErrorRunner;

        #[async_trait]
        impl ConnectorRunner for ErrorRunner {
            async fn run_once(
                &self,
                _bus: EventBus,
                _cache: Arc<RwLock<SnapshotCache>>,
            ) -> anyhow::Result<usize> {
                Err(anyhow::anyhow!("simulated connector error"))
            }
        }

        let bus = common::EventBus::new();
        let cache = Arc::new(RwLock::new(SnapshotCache::new(10, 10)));
        let cancel = CancellationToken::new();

        let task = PollerTask {
            name: "error_task".into(),
            poll_interval: Duration::from_millis(10),
            rate_limiter: RateLimiter::new(100.0, 10.0),
            runner: Box::new(ErrorRunner),
        };

        let mut mgr = PollingManager::new(bus, cache);
        mgr.add_task(task);

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            mgr.run(cancel_clone).await;
        });

        // Errors must not panic — just let it run briefly
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        // No assertion beyond "no panic"
    }
}
