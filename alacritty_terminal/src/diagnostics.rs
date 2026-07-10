use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const DEFAULT_CAPACITY: usize = 8192;
const MARKER_TAIL: usize = 160;

#[derive(Clone, Copy)]
struct Record {
    sequence: u64,
    stage: &'static str,
    ticks: i64,
    bytes: usize,
}

struct Trace {
    run: String,
    path: PathBuf,
    capacity: usize,
    current_sequence: AtomicU64,
    records: Mutex<VecDeque<Record>>,
    scan_tail: Mutex<Vec<u8>>,
}

static TRACE: OnceLock<Option<Trace>> = OnceLock::new();

fn trace() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            let path = env::var_os("ALACRITTY_T013_TRACE").map(PathBuf::from)?;
            let run = env::var("ALACRITTY_T013_RUN_ID").ok()?;
            let capacity = env::var("ALACRITTY_T013_TRACE_CAPACITY")
                .ok()
                .and_then(|capacity| capacity.parse().ok())
                .filter(|capacity| *capacity > 0)
                .unwrap_or(DEFAULT_CAPACITY);
            Some(Trace {
                run,
                path,
                capacity,
                current_sequence: AtomicU64::new(0),
                records: Mutex::new(VecDeque::with_capacity(capacity)),
                scan_tail: Mutex::new(Vec::with_capacity(MARKER_TAIL)),
            })
        })
        .as_ref()
}

fn sequence_from_marker(bytes: &[u8], run: &str) -> Option<u64> {
    sequences_from_markers(bytes, run).into_iter().next()
}

fn sequences_from_markers(bytes: &[u8], run: &str) -> Vec<u64> {
    let prefix = format!("T013:{run}:");
    let mut remaining = bytes;
    let mut sequences = Vec::new();
    while let Some(start) =
        remaining.windows(prefix.len()).position(|window| window == prefix.as_bytes())
    {
        let digits = &remaining[start + prefix.len()..];
        let Some(end) = digits.iter().position(|byte| *byte == b':') else { break };
        if let Ok(sequence) = std::str::from_utf8(&digits[..end]).unwrap_or_default().parse() {
            sequences.push(sequence);
        }
        remaining = &digits[end + 1..];
    }
    sequences
}

fn push(trace: &Trace, record: Record) {
    let mut records = trace.records.lock().unwrap_or_else(|error| error.into_inner());
    if records.len() == trace.capacity {
        records.pop_front();
    }
    records.push_back(record);
}

fn record(trace: &Trace, stage: &'static str, sequence: u64, bytes: usize) {
    if sequence == 0 {
        return;
    }
    push(trace, Record { sequence, stage, ticks: performance_counter(), bytes });
}

pub fn record_bytes(stage: &'static str, bytes: &[u8]) {
    let Some(trace) = trace() else { return };
    let mut tail = trace.scan_tail.lock().unwrap_or_else(|error| error.into_inner());
    tail.extend_from_slice(bytes);
    for sequence in sequences_from_markers(&tail, &trace.run)
        .into_iter()
        .filter(|sequence| *sequence > trace.current_sequence.load(Ordering::Relaxed))
    {
        trace.current_sequence.store(sequence, Ordering::Relaxed);
        record(trace, stage, sequence, bytes.len());
    }
    if tail.len() > MARKER_TAIL {
        let drain = tail.len() - MARKER_TAIL;
        tail.drain(..drain);
    }
}

pub fn record_title(stage: &'static str, title: &str) {
    let Some(trace) = trace() else { return };
    if let Some(sequence) = sequence_from_marker(title.as_bytes(), &trace.run) {
        trace.current_sequence.store(sequence, Ordering::Relaxed);
        record(trace, stage, sequence, title.len());
    }
}

pub fn record_current(stage: &'static str) {
    let Some(trace) = trace() else { return };
    record(trace, stage, trace.current_sequence.load(Ordering::Relaxed), 0);
}

pub fn dump() {
    let Some(trace) = trace() else { return };
    let Ok(file) = File::create(&trace.path) else { return };
    let mut writer = BufWriter::new(file);
    let records = trace.records.lock().unwrap_or_else(|error| error.into_inner());
    for record in records.iter() {
        let _ = writeln!(
            writer,
            "{{\"run\":{:?},\"seq\":{},\"stage\":{:?},\"ticks\":{},\"bytes\":{}}}",
            trace.run, record.sequence, record.stage, record.ticks, record.bytes
        );
    }
}

#[cfg(windows)]
fn performance_counter() -> i64 {
    unsafe extern "system" {
        fn QueryPerformanceCounter(value: *mut i64) -> i32;
    }
    let mut value = 0;
    unsafe { QueryPerformanceCounter(&mut value) };
    value
}

#[cfg(not(windows))]
fn performance_counter() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t013_sequence_parser_accepts_only_matching_markers() {
        let run = "abc123";
        assert_eq!(sequence_from_marker(b"text T013:abc123:42:1000\x1b]0;title", run), Some(42));
        assert_eq!(sequence_from_marker(b"T013:other:42:1000", run), None);
        assert_eq!(sequence_from_marker(b"T013:abc123:not-a-number:1000", run), None);
        assert_eq!(sequences_from_markers(b"T013:abc123:1:10 T013:abc123:2:20", run), vec![1, 2]);
    }
}
