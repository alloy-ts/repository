use std::sync::{Arc, Mutex};

use napi::JsDeferred;
use napi::bindgen_prelude::{
    AsyncBlock, AsyncBlockBuilder, Buffer, Error, JsValue, Promise, PromiseRaw, Status, ToNapiValue,
    Unknown,
};
use napi::{Env, Result};
use napi_derive::napi;

pub use futures::channel;
pub use futures::executor;
pub use futures::future::{self, Future, FutureExt, TryFuture, TryFutureExt};
pub use futures::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt,
    AsyncWrite, AsyncWriteExt,
};
pub use futures::sink::{self, Sink, SinkExt};
pub use futures::stream::{self, Stream, StreamExt, TryStream, TryStreamExt};
pub use futures::task;
pub use futures::{join, never, pending, pin_mut, poll, ready, select, select_biased, try_join};

use crate::thread::ThreadPool;

/// Resolver type used to settle a `u32` `JsDeferred` on the main thread.
type DeferredU32 = Box<dyn FnOnce(Env) -> Result<u32> + Send>;

/// Resolver type used to settle a `Buffer` `JsDeferred` on the main thread.
type DeferredBuffer = Box<dyn FnOnce(Env) -> Result<Buffer> + Send>;

/// Convert a `Debug` error into an `napi::Error`.
fn uv_err<E: std::fmt::Debug>(e: E) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, format!("{e:?}"))
}

/// **Promise-based** (await a JS `Promise` in Rust).
///
/// `Promise<T>` implements `std::future::Future`, so a JS `Promise<u32>` passed
/// from JS can be `.await`ed. Rust does work and the `#[napi] async fn` return
/// value becomes a new JS `Promise` on the way out.
#[napi]
pub async fn async_plus_100(p: Promise<u32>) -> Result<u32> {
    let v = p.await?;
    v.checked_add(100)
        .ok_or_else(|| Error::new(Status::InvalidArg, "result exceeds u32"))
}

/// **PromiseRaw** (sync `.then`/`.catch` on a raw JS `Promise`).
///
/// `PromiseRaw<'env, T>` cannot be `.await`ed and (in this napi version) cannot
/// be returned to JS, so we attach a `then` handler that resolves a `JsDeferred`
/// with the transformed value and return *that* `Promise`.
#[napi(ts_return_type = "Promise<number>")]
pub fn promise_callback(env: Env, promise: PromiseRaw<'static, u32>) -> Result<Unknown<'static>> {
    let (deferred, js_promise) = env.create_deferred::<u32, DeferredU32>().map_err(uv_err)?;
    let slot: Arc<Mutex<Option<JsDeferred<u32, DeferredU32>>>> =
        Arc::new(Mutex::new(Some(deferred)));
    let slot_f = Arc::clone(&slot);
    promise.then(move |ctx| {
        if let Some(d) = slot_f.lock().unwrap().take() {
            d.resolve(Box::new(move |_env: Env| Ok(ctx.value + 100)));
        }
        Ok(())
    })?;

    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_promise)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

/// Create a promise that resolves after `millis` milliseconds.
#[napi(ts_return_type = "Promise<void>")]
pub fn delay_promise(env: Env, millis: f64) -> Result<Unknown<'static>> {
    type DeferredVoid = Box<dyn FnOnce(Env) -> Result<()> + Send>;
    let (deferred, js_promise) = env
        .create_deferred::<(), DeferredVoid>()
        .map_err(uv_err)?;
    let slot: Arc<Mutex<Option<JsDeferred<(), DeferredVoid>>>> =
        Arc::new(Mutex::new(Some(deferred)));

    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs_f64(millis / 1000.0));
        if let Some(d) = slot.lock().unwrap().take() {
            d.resolve(Box::new(|_env: Env| Ok(())));
        }
    });

    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_promise)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

/// Resolve a JS value immediately into a Promise using a `JsDeferred`.
#[napi(ts_return_type = "Promise<unknown>")]
pub fn resolve_value(env: Env, value: Unknown<'static>) -> Result<Unknown<'static>> {
    let (deferred, js_promise) = env
        .create_deferred::<Unknown<'static>, Box<dyn FnOnce(Env) -> Result<Unknown<'static>> + Send>>()
        .map_err(uv_err)?;

    let val_raw = value.raw() as usize;
    deferred.resolve(Box::new(move |env: Env| {
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), val_raw as napi::sys::napi_value) })
    }));

    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_promise)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

/// **AsyncBlock** (return a JS `Promise` built from a Rust future).
///
/// Unlike `Promise<T>` (receive-only), `AsyncBlock<T>` is a manually built future
/// that resolves on the JS thread. Here it resolves with the received `Buffer`,
/// with a no-op `with_dispose` hook that runs when the promise is dropped.
#[napi]
pub fn process_buffer(env: Env, buffer: Buffer) -> Result<AsyncBlock<Buffer>> {
    AsyncBlockBuilder::new(async move { Ok(buffer) })
        .with_dispose(|_env| Ok(()))
        .build(&env)
}

/// **Promise from the libuv thread pool** (`uv_queue_work` + `uv_async_t`).
///
/// Like [`process_buffer`], but the (here: copy) work runs on a libuv worker
/// thread via `ThreadPool::run`. The worker cannot touch V8, so the result is
/// parked in a shared slot and the loop is woken with a `uv_async_t`
/// (`TaskSignal`); the loop-side callback then settles a `JsDeferred`, whose
/// promise is returned to JS. This is the same worker→loop delivery primitive
/// `task.rs` uses to run `Task` programmes.
#[napi(ts_return_type = "Promise<Buffer>")]
pub fn process_buffer_on_pool(env: Env, buffer: Buffer) -> Result<Unknown<'static>> {
    let pool = ThreadPool::new()?;
    let (deferred, js_promise) = env
        .create_deferred::<Buffer, DeferredBuffer>()
        .map_err(uv_err)?;
    let deferred_slot: Arc<Mutex<Option<JsDeferred<Buffer, DeferredBuffer>>>> =
        Arc::new(Mutex::new(Some(deferred)));
    let data = buffer.to_vec();
    pool.run(
        move || data,
        move |data: Vec<u8>| {
            if let Some(d) = deferred_slot.lock().unwrap().take() {
                d.resolve(Box::new(move |_env: Env| Ok(Buffer::from(data))));
            }
        },
    )?;
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_promise)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}
