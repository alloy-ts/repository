use std::sync::Arc;

use napi_derive::napi;

/// A configured git remote, exposed to TypeScript.
#[derive(Clone)]
#[napi(object)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub push_url: Option<String>,
    pub fetch_refspecs: Vec<String>,
    pub push_refspecs: Vec<String>,
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
    let push_url = remote
        .url(gix::remote::Direction::Push)
        .map(|u| u.to_string());
    let fetch_refspecs = remote
        .refspecs(gix::remote::Direction::Fetch)
        .iter()
        .map(|s| s.to_ref().to_bstring().to_string())
        .collect();
    let push_refspecs = remote
        .refspecs(gix::remote::Direction::Push)
        .iter()
        .map(|s| s.to_ref().to_bstring().to_string())
        .collect();
    Remote {
        name,
        url,
        push_url,
        fetch_refspecs,
        push_refspecs,
    }
}
