//! napi bindings for the DataFusion `Task` (from `crate::task::task_service::Task`).
//!
//! **Integration note:** adjust the `use` path if your `Task` lives elsewhere
//! (the snippet imports `crate::task::task_service::Task`). It is exposed
//! as an opaque, cloneable handle.
//!
//! **Why `coordinator_channel` is not bound:** its signature
//! (`HeaderMap`, `SetPlanRequest`, `BoxStream<CoordinatorToTaskMsg>` in;
//! `BoxStream<TaskToCoordinatorMsg>` out) is not JS-friendly (gRPC/protobuf
//! streams). Binding it would require JS-friendly request/response types and a
//! protobuf<->JS bridge.
//!
//! To enable, turn on the `datafusion` feature. Inert by default here.

use std::sync::Arc;

use napi::bindgen_prelude::External;
use napi::{Result as NapiResult, Status};
use napi_derive::napi;

use datafusion::execution::{SendableRecordBatchStream, TaskContext};

use crate::task::task_service::Task as InnerTask;
use crate::ExecuteTaskRequest;

/// Opaque result of [`Task::execute_task`]: the per-partition record-batch
/// streams plus the [`TaskContext`] they were executed against. Returned to JS
/// as an opaque `External` handle.
type ExecuteTaskResult = (Vec<SendableRecordBatchStream>, Arc<TaskContext>);

/// Map any `Display` error (e.g. `DataFusionError`) into an `napi::Error`.
fn to_napi_err<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::new(Status::GenericFailure, e.to_string())
}

/// napi wrapper around the DataFusion [`InnerTask`].
///
/// Cheap to clone (the inner `Task` is `Clone`, all fields `Arc`-backed),
/// which napi relies on for the `async` method below.
#[derive(Clone)]
#[napi]
pub struct Task {
    inner: InnerTask,
}

#[napi]
impl Task {
    // `Task` is normally produced by a `Scheduler` (or a session builder) in
    // your project; there is no `Task::new` in the snippet. Add a
    // `#[napi(constructor)]` / `#[napi(factory)]` here once you have a public
    // constructor, or return `Task` from another napi method (e.g. a
    // `Scheduler` factory) so JS can obtain an instance.

    /// Execute a task, returning the per-partition streams and the
    /// [`TaskContext`] they ran against.
    ///
    /// The request is an opaque `External` handle (mirroring
    /// `crate::ExecuteTaskRequest`); the result is an opaque `External` handle.
    #[napi(ts_return_type = "Promise<unknown>")]
    pub async fn execute_task(
        &self,
        request: External<ExecuteTaskRequest>,
    ) -> NapiResult<External<ExecuteTaskResult>> {
        // `ExecuteTaskRequest` is consumed by the inner call; clone it so the
        // `External` handle can be released. Adjust if your request type is not
        // `Clone` (e.g. wrap it in `Arc` on the caller side).
        let req = (*request).clone();
        let (streams, task_ctx) = self
            .inner
            .execute_task(req)
            .await
            .map_err(to_napi_err)?;
        Ok(External::new((streams, task_ctx)))
    }
}
