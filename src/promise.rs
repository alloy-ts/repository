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

/// Asynchronously delay execution for `millis` milliseconds and resolve with `value`.
#[napi]
pub async fn async_delay(millis: f64, value: u32) -> Result<u32> {
    tokio::time::sleep(std::time::Duration::from_secs_f64(millis / 1000.0)).await;
    Ok(value)
}

/// Await multiple `Promise<u32>` concurrently and return a vector of resolved values.
#[napi]
pub async fn async_all(promises: Vec<Promise<u32>>) -> Result<Vec<u32>> {
    let mut results = Vec::with_capacity(promises.len());
    for p in promises {
        results.push(p.await?);
    }
    Ok(results)
}

/// Await multiple `Promise<u32>` and return the result of the first to resolve.
#[napi]
pub async fn async_race(promises: Vec<Promise<u32>>) -> Result<u32> {
    if promises.is_empty() {
        return Err(Error::new(Status::InvalidArg, "promises array cannot be empty"));
    }
    let futures = promises.into_iter().map(|p| Box::pin(async move { p.await }));
    let (result, _, _) = futures::future::select_all(futures).await;
    result
}

/// Await a promise with a timeout in milliseconds. Rejects if timeout expires.
#[napi]
pub async fn async_timeout(p: Promise<u32>, timeout_ms: f64) -> Result<u32> {
    let duration = std::time::Duration::from_secs_f64(timeout_ms / 1000.0);
    tokio::select! {
        res = p => res,
        _ = tokio::time::sleep(duration) => Err(Error::new(Status::GenericFailure, "Operation timed out")),
    }
}

/// Synchronously create a resolved JS promise with `value`.
#[napi(ts_return_type = "Promise<number>")]
pub fn promise_resolve(env: Env, value: u32) -> Result<Unknown<'static>> {
    let (deferred, js_promise) = env.create_deferred::<u32, DeferredU32>().map_err(uv_err)?;
    deferred.resolve(Box::new(move |_env: Env| Ok(value)));
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_promise)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

/// Synchronously create a rejected JS promise with `reason`.
#[napi(ts_return_type = "Promise<number>")]
pub fn promise_reject(env: Env, reason: String) -> Result<Unknown<'static>> {
    let (deferred, js_promise) = env.create_deferred::<u32, DeferredU32>().map_err(uv_err)?;
    deferred.reject(Error::new(Status::GenericFailure, reason));
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
