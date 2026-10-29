use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use gix::bstr::ByteSlice;
use gix::progress::MessageLevel as GixMessageLevel;
use gix::progress::Unit;

/// The severity of a progress [`message`](crate::progress::ProgressEvent::level).
///
/// Mirrors `gix`'s `prodash::messages::MessageLevel`; exposed as a typed enum so JS
/// consumers can match level values instead of relying on raw strings.
#[napi]
pub enum MessageLevel {
    /// Informational progress, e.g. "Enumerating objects".
    Info,
    /// The step completed successfully.
    Success,
    /// The step failed.
    Failure,
}

/// A progress update emitted from a long-running `fetch`/`clone` operation.
///
/// Forwarded from the server-side progress stream (and gix's own progress tree) to the JS callback
/// supplied to [`Remote::fetch`](crate::remote::Remote::fetch) /
/// [`Repository::clone`](crate::repository::Repository::clone).
#[napi(object)]
pub struct ProgressEvent {
    /// The current progress phase/name, like `"Receiving objects"` or `"Resolving deltas"`.
    pub name: Option<String>,
    /// The number of items processed so far.
    pub step: Option<f64>,
    /// The expected total number of items, if known.
    pub max: Option<f64>,
    /// Completion percentage (0–100), derived from `step`/`max` when both are known.
    pub percent: Option<u32>,
    /// A free-form log message, for `info`/`success`/`failure` events.
    pub message: Option<String>,
    /// The severity of `message`, as a [`MessageLevel`].
    pub level: Option<MessageLevel>,
    /// The unit of `step`/`max`, e.g. `"objects"`. Present only on the `init` event that
    /// opens a progress phase; subsequent updates omit it.
    pub unit: Option<String>,
}

/// Render gix's `Unit` (a label like `"objects"`) to a plain string.
fn unit_label(unit: &Unit) -> String {
    let mut out = String::new();
    let _ = unit
        .as_display_value()
        .display_current_value(&mut out, 0, None);
    out
}

/// Bridges a JS progress callback into the `gix` [`Progress`](gix::progress::Progress) trait.
///
/// `ThreadsafeFunction` lets the task thread invoke the JS callback safely. Children created via
/// `add_child` share the same callback.
#[derive(Clone)]
pub(crate) struct ProgressCallback {
    tsfn: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    step: Arc<AtomicUsize>,
}

impl ProgressCallback {
    pub(crate) fn new(tsfn: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>) -> Self {
        ProgressCallback {
            tsfn,
            step: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn emit(&self, mut event: ProgressEvent) {
        if event.percent.is_none() {
            if let (Some(s), Some(m)) = (event.step, event.max) {
                if m > 0.0 {
                    event.percent = Some((s / m * 100.0).clamp(0.0, 100.0) as u32);
                }
            }
        }
        if let Some(tsfn) = &self.tsfn {
            let _ = tsfn.call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);
        }
    }
}

impl gix::progress::Count for ProgressCallback {
    fn set(&self, step: usize) {
        self.step.store(step, Ordering::SeqCst);
        self.emit(ProgressEvent {
            name: None,
            step: Some(step as f64),
            max: None,
            percent: None,
            message: None,
            level: None,
            unit: None,
        });
    }

    fn step(&self) -> usize {
        self.step.load(Ordering::SeqCst)
    }

    fn inc_by(&self, step: usize) {
        let v = self.step.fetch_add(step, Ordering::SeqCst) + step;
        self.emit(ProgressEvent {
            name: None,
            step: Some(v as f64),
            max: None,
            percent: None,
            message: None,
            level: None,
            unit: None,
        });
    }

    fn counter(&self) -> gix::progress::StepShared {
        self.step.clone()
    }
}

impl gix::progress::NestedProgress for ProgressCallback {
    type SubProgress = ProgressCallback;

    fn add_child(&mut self, name: impl Into<String>) -> Self::SubProgress {
        let child = ProgressCallback::new(self.tsfn.clone());
        child.emit(ProgressEvent {
            name: Some(name.into()),
            step: None,
            max: None,
            percent: None,
            message: None,
            level: None,
            unit: None,
        });
        child
    }

    fn add_child_with_id(
        &mut self,
        name: impl Into<String>,
        _id: gix::progress::Id,
    ) -> Self::SubProgress {
        self.add_child(name)
    }
}

impl gix::progress::Progress for ProgressCallback {
    fn init(&mut self, max: Option<usize>, _unit: Option<Unit>) {
        self.emit(ProgressEvent {
            name: None,
            step: None,
            max: max.map(|m| m as f64),
            percent: None,
            message: None,
            level: None,
            unit: _unit.as_ref().map(unit_label),
        });
    }

    fn set_name(&mut self, name: String) {
        self.emit(ProgressEvent {
            name: Some(name),
            step: None,
            max: None,
            percent: None,
            message: None,
            level: None,
            unit: None,
        });
    }

    fn name(&self) -> Option<String> {
        None
    }

    fn id(&self) -> gix::progress::Id {
        gix::progress::UNKNOWN
    }

    fn message(&self, level: GixMessageLevel, message: String) {
        let level = match level {
            GixMessageLevel::Info => MessageLevel::Info,
            GixMessageLevel::Success => MessageLevel::Success,
            GixMessageLevel::Failure => MessageLevel::Failure,
        };
        self.emit(ProgressEvent {
            name: None,
            step: None,
            max: None,
            percent: None,
            message: Some(message),
            level: Some(level),
            unit: None,
        });
    }
}

/// Parsed progress information as sent by a git server during fetch, clone and push operations.
///
/// Build one from a raw server progress `line` with [`RemoteProgress::from_bytes`].
#[napi]
pub struct RemoteProgress {
    /// The name of the action, like `"Receiving objects"` or `"Resolving deltas"`.
    pub action: String,
    /// The completion percentage (0–100), if the server reported one.
    pub percent: Option<u32>,
    /// The number of items already processed, if reported.
    pub step: Option<f64>,
    /// The maximum expected number of items (`step` / `max` * 100 ≈ `percent`), if reported.
    pub max: Option<f64>,
    /// Total bytes transferred so far, parsed from the trailing throughput portion of the line
    /// (e.g. `, 1.23 MiB`). `null` when the server omitted it. This is git2's
    /// `Progress::received_bytes` equivalent.
    pub received_bytes: Option<f64>,
    /// Transfer rate in bytes per second, parsed from the trailing `| x MiB/s` portion. `null`
    /// when the server omitted it.
    pub throughput: Option<f64>,
}

/// Parse git-style human-readable byte counts from a server progress line, e.g.
/// `, 1.23 MiB | 4.56 MiB/s` → `(Some(1289748.48), Some(4781506.56))`.
///
/// The first count is the total transferred so far; the second (after `|`) is the rate. Units use
/// git's 1024-base suffixes (`B`, `KiB`, `MiB`, `GiB`, `TiB`, `PiB`, `EiB`). Returns
/// `(None, None)` when no such tokens are present (the common case for phase lines without
/// throughput).
fn parse_byte_counts(text: &str) -> (Option<f64>, Option<f64>) {
    const UNITS: &[(&str, f64)] = &[
        ("EiB", (1u64 << 60) as f64),
        ("PiB", (1u64 << 50) as f64),
        ("TiB", (1u64 << 40) as f64),
        ("GiB", (1u64 << 30) as f64),
        ("MiB", (1u64 << 20) as f64),
        ("KiB", (1u64 << 10) as f64),
        ("B", 1.0),
    ];
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        let mut j = i;
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }
        let unit_start = j;
        let mut k = j;
        while k < bytes.len() && bytes[k].is_ascii_alphabetic() {
            k += 1;
        }
        let unit = &text[unit_start..k];
        if let Some((_, mult)) = UNITS.iter().find(|(u, _)| u.eq_ignore_ascii_case(unit)) {
            if let Ok(value) = text[start..i].parse::<f64>() {
                found.push(value * mult);
            }
        }
        i = k;
    }
    (found.first().copied(), found.get(1).copied())
}

#[napi]
impl RemoteProgress {
    /// Parse a remote progress `line` (as sent by the git server) into structured progress.
    ///
    /// Returns `null` if the line does not contain recognizable progress information
    /// (e.g. a keep-alive packet or a line without an action prefix).
    #[napi]
    pub fn from_bytes(text: String) -> Option<RemoteProgress> {
        let (received_bytes, throughput) = parse_byte_counts(&text);
        gix::protocol::RemoteProgress::from_bytes(text.as_bytes()).map(|p| RemoteProgress {
            action: String::from_utf8_lossy(p.action.as_bytes()).into_owned(),
            percent: p.percent,
            step: p.step.map(|s| s as f64),
            max: p.max.map(|m| m as f64),
            received_bytes,
            throughput,
        })
    }
}
