//! Runs the collector and persists what it reports.
//!
//! The collector's callback fires on its own message-loop thread, which also drives
//! the WinEvent hook. Writing to disk there would stall event delivery, so the sink
//! does nothing but hand the event to a writer thread over a channel. The writer owns
//! the [`EventStore`] and is the only thing that touches it.
//!
//! Nothing in this module refers to Tauri, so the whole pipeline — collector to
//! channel to writer to JSONL — is exercised by an ordinary integration test.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use oh_collector::{Collector, start_collector};
use oh_core::{ActivityEvent, Config, EventStore, local_date_of, paths, today};
use parking_lot::Mutex;
use serde::Serialize;

/// What the application is doing right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub events_today: usize,
    /// Timestamp of the most recent recorded event, exactly as written.
    pub last_event_at: Option<String>,
    /// Where history is being kept, so the interface can show it and open it.
    pub data_dir: String,
}

/// Called whenever the status changes, so the interface can follow along.
pub type StatusListener = Arc<dyn Fn(&Status) + Send + Sync>;

pub struct CollectorService {
    running: Mutex<Option<Running>>,
    status: Arc<Mutex<Status>>,
    listener: StatusListener,
}

struct Running {
    collector: Collector,
    events: Sender<ActivityEvent>,
    writer: JoinHandle<()>,
}

impl CollectorService {
    pub fn new(listener: StatusListener) -> Self {
        let data_dir = paths::data_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        CollectorService {
            running: Mutex::new(None),
            status: Arc::new(Mutex::new(Status {
                data_dir,
                ..Status::default()
            })),
            listener,
        }
    }

    /// A service with no listener, for tests and for the headless probe.
    pub fn detached() -> Self {
        Self::new(Arc::new(|_: &Status| {}))
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }

    /// Start collecting. Starting an already-running service does nothing.
    pub fn start(&self, config: &Config) -> Result<()> {
        let mut running = self.running.lock();
        if running.is_some() {
            return Ok(());
        }

        paths::ensure_layout()?;
        let store = EventStore::open().context("could not open the event log")?;
        if config.retention_days > 0 {
            match store.prune(config.retention_days) {
                Ok(removed) if !removed.is_empty() => {
                    tracing::info!(days = removed.len(), "pruned expired history")
                }
                Err(error) => tracing::warn!(%error, "could not prune expired history"),
                _ => {}
            }
        }

        // Seed the day's count from what is already on disk, so restarting the
        // collector mid-day does not make the interface claim the day started now.
        let day = today();
        let seed = store.stats(day).unwrap_or_default();
        {
            let mut status = self.status.lock();
            status.running = true;
            status.events_today = seed.events;
            status.last_event_at = seed.last_event_at;
        }

        let (events, incoming) = mpsc::channel::<ActivityEvent>();
        let writer = spawn_writer(
            store,
            day,
            incoming,
            Arc::clone(&self.status),
            Arc::clone(&self.listener),
        );

        let sink = {
            let events = events.clone();
            Box::new(move |event: ActivityEvent| {
                // A closed channel means the writer has already shut down, which
                // happens between `stop` tearing down the writer and the collector
                // thread noticing. Dropping the event is correct there.
                let _ = events.send(event);
            })
        };

        let collector: Collector = start_collector(sink, config.recording.clone())
            .context("could not start the collector")?;

        *running = Some(Running {
            collector,
            events,
            writer,
        });
        drop(running);

        self.announce();
        Ok(())
    }

    /// Stop collecting and flush everything already observed.
    pub fn stop(&self) {
        let Some(running) = self.running.lock().take() else {
            return;
        };

        // Order matters. Stopping the collector first guarantees no further sends;
        // dropping the sender then ends the writer's loop, and joining it means every
        // event the collector produced is on disk before this returns.
        running.collector.stop();
        drop(running.events);
        if running.writer.join().is_err() {
            tracing::error!("the event writer thread panicked");
        }

        self.status.lock().running = false;
        self.announce();
    }

    /// Apply changed settings, restarting the collector if it is running.
    ///
    /// The exclusion list is read once when the collector starts, so a change to it
    /// only takes effect on restart.
    pub fn reconfigure(&self, config: &Config) -> Result<()> {
        let was_running = self.is_running();
        self.stop();
        if was_running && config.recording_enabled {
            self.start(config)?;
        }
        Ok(())
    }

    fn announce(&self) {
        let status = self.status();
        (self.listener)(&status);
    }
}

impl Drop for CollectorService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The only thread that writes to the event log.
///
/// It ends when the sending half of `incoming` is dropped, which is how `stop`
/// guarantees a clean flush rather than killing the thread mid-append.
fn spawn_writer(
    mut store: EventStore,
    mut counted_day: NaiveDate,
    incoming: Receiver<ActivityEvent>,
    status: Arc<Mutex<Status>>,
    listener: StatusListener,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("oh-event-writer".into())
        .spawn(move || {
            for event in incoming {
                let day = local_date_of(&event);
                if let Err(error) = store.append(&event) {
                    // A failed append must not stop collection. The user would rather
                    // lose one event to a full disk than lose the rest of the day.
                    tracing::error!(%error, kind = ?event.kind, "could not record an event");
                    continue;
                }

                let snapshot = {
                    let mut status = status.lock();
                    if day != counted_day {
                        counted_day = day;
                        status.events_today = 0;
                    }
                    status.events_today += 1;
                    status.last_event_at = Some(event.timestamp.clone());
                    status.clone()
                };
                listener(&snapshot);
            }
        })
        .expect("the event writer thread must spawn")
}
