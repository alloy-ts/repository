use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use gix::bstr::ByteSlice;
use gix::progress::MessageLevel as GixMessageLevel;
use gix::progress::Unit;
use gix::protocol;

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
}

/// Bridges a JS progress callback into the `gix` [`Progress`](gix::progress::Progress) trait.
///
/// `ThreadsafeFunction` lets the worker thread invoke the JS callback safely. Children created via
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
        });
        child
    }

    fn add_child_with_id(&mut self, name: impl Into<String>, _id: gix::progress::Id) -> Self::SubProgress {
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
        });
    }
}

/// Parsed progress information as sent by a git server during fetch, clone and push operations.
///
/// Build one from a raw server progress `line` with [`RemoteProgress::from_bytes`].
#[napi(object)]
pub struct RemoteProgress {
    /// The name of the action, like `"Receiving objects"` or `"Resolving deltas"`.
    pub action: String,
    /// The completion percentage (0–100), if the server reported one.
    pub percent: Option<u32>,
    /// The number of items already processed, if reported.
    pub step: Option<f64>,
    /// The maximum expected number of items (`step` / `max` * 100 ≈ `percent`), if reported.
    pub max: Option<f64>,
}

#[napi]
impl RemoteProgress {
    /// Parse a remote progress `line` (as sent by the git server) into structured progress.
    ///
    /// Returns `null` if the line does not contain recognizable progress information
    /// (e.g. a keep-alive packet or a line without an action prefix).
    #[napi]
    pub fn from_bytes(text: String) -> Option<RemoteProgress> {
        gix::protocol::RemoteProgress::from_bytes(text.as_bytes()).map(|p| RemoteProgress {
            action: String::from_utf8_lossy(p.action.as_bytes()).into_owned(),
            percent: p.percent,
            step: p.step.map(|s| s as f64),
            max: p.max.map(|m| m as f64),
        })
    }
}
