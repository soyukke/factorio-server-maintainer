//! Incremental log-file tail with rotation awareness — see spec §6.4 and §6.5.
//!
//! The parser that converts lines into `ServerEvent`s is game-specific and
//! lives in the per-game crate.
//!
//! Behaviour:
//! - If the file doesn't exist yet, keep polling until it does.
//! - On every poll, compare current size to last read position. If the file
//!   shrank, treat it as rotation/truncation and re-seek to 0.
//! - Read whatever new bytes are present, split on `\n`, trim trailing CR,
//!   forward each complete line. Partial trailing data stays buffered for the
//!   next poll.

use std::path::PathBuf;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

#[derive(Clone, Debug)]
pub struct LogTailConfig {
    pub path: PathBuf,
    pub poll_interval: Duration,
    pub channel_capacity: usize,
    /// Initial cursor position. `0` reads from the start; pass the current
    /// file size to skip historical content (re-attach use case).
    pub start_pos: u64,
}

impl LogTailConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            poll_interval: Duration::from_millis(250),
            channel_capacity: 1024,
            start_pos: 0,
        }
    }
}

/// Spawn a tail task. Stops on receiver drop or `cancel` token being aborted.
/// Returns the receiver and the task handle so callers can `abort()` on stop.
pub fn spawn(cfg: LogTailConfig) -> (mpsc::Receiver<String>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<String>(cfg.channel_capacity);
    let handle = tokio::spawn(run(cfg, tx));
    (rx, handle)
}

async fn run(cfg: LogTailConfig, tx: mpsc::Sender<String>) {
    let mut buf = Vec::<u8>::new();
    let mut pos: u64 = cfg.start_pos;

    loop {
        if tx.is_closed() {
            return;
        }

        let mut file = match File::open(&cfg.path).await {
            Ok(f) => f,
            Err(_) => {
                sleep(cfg.poll_interval).await;
                continue;
            }
        };

        let size = match file.metadata().await {
            Ok(m) => m.len(),
            Err(_) => {
                sleep(cfg.poll_interval).await;
                continue;
            }
        };

        // Rotation / truncation: file shrank below our read cursor.
        if size < pos {
            tracing::debug!(
                path = %cfg.path.display(),
                pos, size,
                "log file shrank, resetting tail cursor"
            );
            pos = 0;
            buf.clear();
        }

        if size > pos {
            if file.seek(SeekFrom::Start(pos)).await.is_err() {
                sleep(cfg.poll_interval).await;
                continue;
            }
            let mut chunk = Vec::new();
            if file.read_to_end(&mut chunk).await.is_err() {
                sleep(cfg.poll_interval).await;
                continue;
            }
            pos += chunk.len() as u64;
            buf.extend_from_slice(&chunk);

            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                let line_bytes: Vec<u8> = buf.drain(..=nl).collect();
                let trimmed_len = line_bytes
                    .iter()
                    .rposition(|b| *b != b'\n' && *b != b'\r')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let line = String::from_utf8_lossy(&line_bytes[..trimmed_len]).into_owned();
                if tx.send(line).await.is_err() {
                    return;
                }
            }
        }

        sleep(cfg.poll_interval).await;
    }
}
