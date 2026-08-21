//! Streams collector output as JSON Lines, one event per line.
//!
//! ```text
//! cargo run -p oh-collector --example probe -- 30
//! ```
//!
//! Switch applications, open a browser tab, open a private window. Each observation
//! prints as it happens, in exactly the form that reaches disk.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use oh_collector::{CollectorConfig, start_collector};

fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(30);

    let (tx, events) = mpsc::channel();
    let collector = start_collector(
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
        CollectorConfig::default(),
    )?;

    eprintln!("recording for {seconds}s; switch applications to produce events");

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut count = 0usize;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(event) => {
                println!("{}", serde_json::to_string(&event)?);
                count += 1;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    collector.stop();
    eprintln!("{count} events");
    Ok(())
}
