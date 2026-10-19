mod clone;
mod error;
mod object;
mod reference;
mod remote;
mod repository;
mod util;

use napi_derive::napi;

pub use clone::*;
pub use object::*;
pub use reference::*;
pub use remote::*;
pub use repository::*;
pub use util::*;

#[napi]
pub fn add(left: i64, right: i64) -> i64 {
    left + right
}
