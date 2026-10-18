mod error;
mod object;
mod reference;
mod remote;
mod repository;

use napi_derive::napi;

pub use object::*;
pub use reference::*;
pub use remote::*;
pub use repository::*;

#[napi]
pub fn add(left: i64, right: i64) -> i64 {
    left + right
}
