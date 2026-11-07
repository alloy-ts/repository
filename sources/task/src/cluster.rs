//! Draft process-pool binding. Each fork starts a separate executable with its
//! own address space. This does not yet implement Node cluster IPC, event
//! forwarding, handle passing, shared server ports, or scheduling policies.
//! `Task` in the parent module is a libuv task, not a subprocess; descriptors
//! here use CommandOptions instead of attempting to migrate a live Task.

use std::collections::HashMap;
use napi_derive::napi;
use super::process::{CancellationToken, CommandOptions, EnvEntry, Process, create_cancellation_token};

/// Primary-side registry of process cancellation handles. Dropping the cluster
/// requests cleanup of all its children. Returned Process objects collect output.
#[napi]
pub struct Cluster {
    next_id: u32,
    tasks: HashMap<u32, CancellationToken>,
}

#[napi]
impl Cluster {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self { next_id: 1, tasks: HashMap::new() }
    }

    /// Explicit commands avoid recursively re-running the host application.
    /// ALLOY_CLUSTER_TASK_ID identifies the child without claiming Node IPC.
    #[napi]
    pub fn fork(&mut self, mut options: CommandOptions) -> napi::Result<Process> {
        let id = self.next_id;
        let next = id.checked_add(1).ok_or_else(|| napi::Error::from_reason("cluster task IDs exhausted"))?;
        let env = options.env.get_or_insert_with(Vec::new);
        env.retain(|entry| entry.name != "ALLOY_CLUSTER_TASK_ID");
        env.push(EnvEntry { name: "ALLOY_CLUSTER_TASK_ID".into(), value: id.to_string() });
        let token = create_cancellation_token();
        let process = Process::start(options, Some(token))?;
        self.tasks.insert(id, token);
        self.next_id = next;
        Ok(process)
    }

    /// IDs registered by this instance; call remove after collecting a result.
    #[napi(getter)]
    pub fn task_ids(&self) -> Vec<u32> {
        let mut ids: Vec<_> = self.tasks.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Cancel a registered child. This is not a graceful IPC disconnect.
    #[napi]
    pub fn cancel(&self, id: u32) -> bool {
        if let Some(token) = self.tasks.get(&id) {
            token.cancel();
            true
        } else { false }
    }

    /// Remove a registration and request cleanup if it is still running.
    #[napi]
    pub fn remove(&mut self, id: u32) -> bool {
        if let Some(token) = self.tasks.remove(&id) {
            token.cancel();
            true
        } else { false }
    }

    #[napi]
    pub fn cancel_all(&self) {
        for token in self.tasks.values() { token.cancel(); }
    }
}

impl Drop for Cluster {
    fn drop(&mut self) { self.cancel_all(); }
}
