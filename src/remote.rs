use std::sync::Arc;

use napi_derive::napi;

/// A configured git remote, exposed to TypeScript.
#[napi(object)]
pub struct Remote {
    pub name: String,
    /// Primary fetch URL.
    pub url: String,
    /// Primary push URL (falls back to fetch URL if no explicit push URL configured).
    pub push_url: String,
    /// All configured fetch URLs.
    pub fetch_urls: Vec<String>,
    /// All configured explicit push URLs.
    pub push_urls: Vec<String>,
    /// Configured fetch refspecs.
    pub fetch_refspecs: Vec<String>,
    /// Configured push refspecs.
    pub push_refspecs: Vec<String>,
    /// Tag option strategy ("all", "none", "included").
    pub fetch_tags: String,
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
        .map(|u| u.to_string())
        .unwrap_or_default();

    let push_url = remote
        .url(gix::remote::Direction::Push)
        .map(|u| u.to_string())
        .unwrap_or_default();

    let fetch_urls = remote
        .urls(gix::remote::Direction::Fetch)
        .map(|u| u.to_string())
        .collect();

    let push_urls = remote
        .urls(gix::remote::Direction::Push)
        .map(|u| u.to_string())
        .collect();

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

    let fetch_tags = match remote.fetch_tags() {
        gix::remote::fetch::Tags::All => "all".to_string(),
        gix::remote::fetch::Tags::None => "none".to_string(),
        gix::remote::fetch::Tags::Included => "included".to_string(),
    };

    Remote {
        name,
        url,
        push_url,
        fetch_urls,
        push_urls,
        fetch_refspecs,
        push_refspecs,
        fetch_tags,
    }
}
