use std::sync::Arc;

use napi_derive::napi;

/// A configured git remote, exposed to TypeScript.
#[napi(object)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

pub fn build_remote(
    _repo: &Arc<gix::ThreadSafeRepository>,
    remote: gix::Remote,
) -> Remote {
    let name = remote
        .name()
        .map(|n| n.as_bstr().to_string())
        .unwrap_or_default();
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .or_else(|| remote.url(gix::remote::Direction::Push))
        .map(|u| u.to_string())
        .unwrap_or_default();
    Remote { name, url }
}
