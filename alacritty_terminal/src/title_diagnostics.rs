//! Bounded, asynchronous diagnostics for the Windows title delivery investigation.

use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RECORD_LIMIT: usize = 2048;
const FILE_LIMIT: u64 = 4 * 1024 * 1024;
const HEARTBEAT: Duration = Duration::from_secs(5);

static LOGGER: OnceLock<Logger> = OnceLock::new();

#[derive(Default)]
struct Counters {
    pty_bytes: AtomicU64,
    gui_ticks: AtomicU64,
    dropped: AtomicU64,
}

struct Logger {
    sender: SyncSender<String>,
    counters: Arc<Counters>,
    start: Instant,
}

/// Queue one bounded record without waiting for the diagnostic file writer.
pub fn record(stage: &str, detail: fmt::Arguments<'_>) {
    let logger = logger();
    let mut line = BoundedText(String::with_capacity(RECORD_LIMIT));
    let _ = write!(line, "t_us={} stage={} {}", logger.start.elapsed().as_micros(), stage, detail);
    // Keep each record on one line, including Debug strings containing raw newlines.
    let mut line = line.0;
    line = line.replace(['\r', '\n'], " ");
    line.push('\n');
    if logger.sender.try_send(line).is_err() {
        logger.counters.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Count PTY bytes without recording their contents.
pub fn pty_read(bytes: usize) {
    logger().counters.pty_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// Count GUI event-loop boundaries independently of title records.
pub fn event_loop_tick() {
    logger().counters.gui_ticks.fetch_add(1, Ordering::Relaxed);
}

fn logger() -> &'static Logger {
    LOGGER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(256);
        let counters = Arc::new(Counters::default());
        let writer_counters = counters.clone();
        let start = Instant::now();
        let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        // Failure disconnects the channel; callers continue through try_send without panicking.
        let _ = std::thread::Builder::new().name("title-diagnostics".into()).spawn(move || {
            let _ = writer(receiver, writer_counters, start, epoch);
        });
        Logger { sender, counters, start }
    })
}

struct BoundedText(String);

impl fmt::Write for BoundedText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let remaining = RECORD_LIMIT.saturating_sub(self.0.len());
        let mut end = remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&text[..end]);
        if end < text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}

struct RotatingFile {
    path: PathBuf,
    previous: PathBuf,
    file: Option<File>,
    size: u64,
    limit: u64,
}

impl RotatingFile {
    fn create(directory: PathBuf, name: &str, limit: u64) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{name}.log"));
        let previous = directory.join(format!("{name}.previous.log"));
        let file = OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok(Self { path, previous, file: Some(file), size: 0, limit })
    }

    fn write_record(&mut self, record: &str) -> io::Result<()> {
        if record.len() as u64 > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic record exceeds limit",
            ));
        }
        if self.size + record.len() as u64 > self.limit {
            // Close before rename on Windows. Never keep more than these two files.
            self.file.take();
            match fs::remove_file(&self.previous) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            fs::rename(&self.path, &self.previous)?;
            self.file = Some(OpenOptions::new().write(true).create_new(true).open(&self.path)?);
            self.size = 0;
        }
        let file = self.file.as_mut().ok_or_else(|| io::Error::other("diagnostic file closed"))?;
        file.write_all(record.as_bytes())?;
        self.size += record.len() as u64;
        Ok(())
    }
}

fn heartbeat_due(now: Instant, deadline: &mut Instant, interval: Duration) -> bool {
    if now < *deadline {
        return false;
    }
    // Move from the absolute deadline, so incoming records cannot postpone the heartbeat.
    while *deadline <= now {
        *deadline += interval;
    }
    true
}

fn writer(
    receiver: Receiver<String>,
    counters: Arc<Counters>,
    start: Instant,
    epoch: u128,
) -> io::Result<()> {
    let name = format!("title-{}-{epoch}", std::process::id());
    let fallback = || {
        RotatingFile::create(std::env::temp_dir().join("Alacritty/diagnostics"), &name, FILE_LIMIT)
    };
    let mut file = match std::env::var_os("LOCALAPPDATA") {
        Some(directory) => RotatingFile::create(
            PathBuf::from(directory).join("Alacritty/diagnostics"),
            &name,
            FILE_LIMIT,
        )
        .or_else(|_| fallback())?,
        None => fallback()?,
    };
    let mut init = BoundedText(String::new());
    let _ = write!(
        init,
        "stage=init epoch_start_ms={epoch} pid={} exe={:?}",
        std::process::id(),
        std::env::current_exe()
    );
    init.0.push('\n');
    file.write_record(&init.0)?;
    let mut deadline = start + HEARTBEAT;
    loop {
        let now = Instant::now();
        if heartbeat_due(now, &mut deadline, HEARTBEAT) {
            file.write_record(&format!(
                "t_us={} epoch_start_ms={epoch} stage=heartbeat pty_bytes={} gui_ticks={} dropped={}\n",
                now.duration_since(start).as_micros(),
                counters.pty_bytes.load(Ordering::Relaxed),
                counters.gui_ticks.load(Ordering::Relaxed),
                counters.dropped.load(Ordering::Relaxed),
            ))?;
        }
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(record) => file.write_record(&record)?,
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_bounded_at_utf8_boundaries() {
        let mut text = BoundedText(String::new());
        let _ = write!(text, "{}{}", "a".repeat(RECORD_LIMIT - 1), "⠿".repeat(100));
        assert_eq!(text.0.len(), RECORD_LIMIT - 1);
        assert!(text.0.is_char_boundary(text.0.len()));
    }

    #[test]
    fn incoming_records_do_not_postpone_heartbeat() {
        let start = Instant::now();
        let mut deadline = start + HEARTBEAT;
        let mut heartbeats = 0;
        for milliseconds in 0..=15_000 {
            heartbeats += usize::from(heartbeat_due(
                start + Duration::from_millis(milliseconds),
                &mut deadline,
                HEARTBEAT,
            ));
        }
        assert_eq!(heartbeats, 3);
        assert_eq!(deadline, start + Duration::from_secs(20));
    }

    #[test]
    fn repeated_rotation_keeps_only_two_bounded_files() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir()
            .join(format!("alacritty-title-test-{}-{nonce}", std::process::id()));
        let mut file = RotatingFile::create(directory.clone(), "test", 16).unwrap();
        for _ in 0..20 {
            file.write_record("1234567\n").unwrap();
        }
        drop(file);
        let entries: Vec<_> = fs::read_dir(&directory).unwrap().map(Result::unwrap).collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.metadata().unwrap().len() <= 16));
        assert_eq!(fs::read_to_string(directory.join("test.log")).unwrap(), "1234567\n1234567\n");
        fs::remove_file(directory.join("test.log")).unwrap();
        fs::remove_file(directory.join("test.previous.log")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
