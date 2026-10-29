//! NAPI bindings for libuv's base request type.
//!
//! Exposes [`Req`], a thin wrapper around `libuv::Req` providing `cancel`
//! (`uv_cancel`) and `type_name`. Most libuv request types (`WorkReq`, `FsReq`,
//! `WriteReq`, …) are also a `Req` and can be cancelled through it.

use libuv::ReqTrait;
use napi::{Error, Result, Status};
use napi_derive::napi;

/// A thin wrapper around libuv's base request type (`uv_req_t`).
///
/// Most libuv request types (`WorkReq`, `FsReq`, `WriteReq`, …) are also a
/// `Req` and can be cancelled via `uv_cancel` or queried for their type.
#[napi]
pub struct Req {
    inner: libuv::Req,
}

// `libuv::Req` wraps a raw `uv_req_t*` and is `!Send`/`!Sync`, but a `Req` is
// only ever used on the main (V8/JS) thread, so marking it `Send`/`Sync` is
// sound.
unsafe impl Send for Req {}
unsafe impl Sync for Req {}

impl From<libuv::Req> for Req {
    fn from(inner: libuv::Req) -> Self {
        Req { inner }
    }
}

#[napi]
impl Req {
    /// Cancel the underlying request (`uv_cancel`).
    ///
    /// Only requests that have not yet started can be cancelled. If the request
    /// is already executing or has finished, this fails. Cancelled requests have
    /// their callbacks invoked later; do not free the request until then. The
    /// libuv docs note that only `WorkReq`, `FsReq`, `GetAddrInfoReq`,
    /// `GetNameInfoReq`, `RandomReq` and `WriteReq` support cancellation.
    #[napi]
    pub fn cancel(&mut self) -> Result<()> {
        self.inner
            .cancel()
            .map_err(|e| Error::new(Status::GenericFailure, format!("{e:?}")))
    }

    /// The name of the request type (e.g. `"WORK"`, `"FS"`).
    #[napi]
    pub fn type_name(&self) -> String {
        self.inner.get_type().name()
    }
}
