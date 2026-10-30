use std::sync::{Arc, Mutex};

use napi::JsDeferred;
use napi::bindgen_prelude::{
    AsyncBlock, AsyncBlockBuilder, Buffer, Error, Promise, PromiseRaw, Status, ToNapiValue, Unknown,
};
use napi::{Env, Result};
use napi_derive::napi;

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

/// **Promise from the libuv thread pool** (`uv_queue_work`).
///
/// Like [`process_buffer`], but the (here: copy) work runs on a libuv worker
/// thread via `ThreadPool::run`. The worker cannot touch V8, so the result is
/// delivered directly to the loop thread in the `uv_after_work_cb` callback,
/// which settles a `JsDeferred` whose promise is returned to JS.
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
        move |data: Option<Vec<u8>>, status: std::result::Result<u32, libuv::Error>| {
            if let Some(d) = deferred_slot.lock().unwrap().take() {
                if status.is_ok() {
                    if let Some(data) = data {
                        d.resolve(Box::new(move |_env: Env| Ok(Buffer::from(data))));
                        return;
                    }
                }
                d.resolve(Box::new(move |_env: Env| {
                    Err(Error::new(Status::GenericFailure, "work cancelled or failed"))
                }));
            }
        },
    )?;
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_promise)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}
