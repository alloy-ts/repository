use std::sync::Arc;

use napi::Env;
use napi::Result;
use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Function};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::progress::{ProgressCallback, ProgressEvent};
use crate::repository::Repository;

use gix::protocol;

/// How tags are handled when fetching from a remote.
#[napi]
pub enum FetchTags {
    All,
    None,
    Included,
}

fn to_fetch_tags(t: gix::remote::fetch::Tags) -> FetchTags {
    match t {
        gix::remote::fetch::Tags::All => FetchTags::All,
        gix::remote::fetch::Tags::None => FetchTags::None,
        gix::remote::fetch::Tags::Included => FetchTags::Included,
    }
}

/// Direction for [`Repository::find_default_remote`].
#[napi]
pub enum RemoteDirection {
    /// The remote used for fetching.
    Fetch,
    /// The remote used for pushing.
    Push,
}

pub(crate) fn to_remote_direction(d: RemoteDirection) -> gix::remote::Direction {
    match d {
        RemoteDirection::Fetch => gix::remote::Direction::Fetch,
        RemoteDirection::Push => gix::remote::Direction::Push,
    }
}

/// Acquire a live `gix::Remote` for this binding, reconstructing anonymous remotes from their
/// cached URL, and run `f` against it. The borrowed remote never escapes, avoiding lifetime issues.
fn with_remote<T, F>(
    self_repo: &Arc<gix::ThreadSafeRepository>,
    name: &Option<String>,
    url: &Option<String>,
    f: F,
) -> Result<T>
where
    F: FnOnce(&gix::Remote) -> Result<T>,
{
    let r = thread_local(&self_repo);
    let remote = match name {
        Some(name) => r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?,
        None => {
            let url = url
                .as_deref()
                .ok_or_else(|| err_str("remote", "anonymous remote has no url"))?;
            r.remote_at(url).map_err(|e| err("remote_at", e))?
        }
    };
    f(&remote)
}

/// A configured git remote, exposed to TypeScript.
#[napi]
pub struct Remote {
    repo: Arc<gix::ThreadSafeRepository>,
    name: Option<String>,
    url: Option<String>,
    push_url: Option<String>,
}

#[napi]
impl Remote {
    /// The configured name of this remote, or `null` for an anonymous remote.
    #[napi(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The fetch URL, falling back to the push URL if no fetch URL is configured.
    #[napi(getter)]
    pub fn url(&self) -> Option<String> {
        self.url.clone()
    }

    /// The explicit push URL, or `null` if none is configured.
    #[napi(getter)]
    pub fn push_url(&self) -> Option<String> {
        self.push_url.clone()
    }

    /// How tags are handled when fetching from this remote.
    #[napi(getter)]
    pub fn fetch_tags(&self) -> Result<FetchTags> {
        with_remote(&self.repo, &self.name, &self.url, |remote| {
            Ok(to_fetch_tags(remote.fetch_tags()))
        })
    }

    /// The configured fetch refspecs, in configuration order.
    #[napi(getter)]
    pub fn fetch_refspecs(&self) -> Result<Vec<String>> {
        with_remote(&self.repo, &self.name, &self.url, |remote| {
            Ok(remote
                .refspecs(gix::remote::Direction::Fetch)
                .iter()
                .map(|s| s.to_ref().to_bstring().to_string())
                .collect())
        })
    }

    /// The configured push refspecs, in configuration order.
    #[napi(getter)]
    pub fn push_refspecs(&self) -> Result<Vec<String>> {
        with_remote(&self.repo, &self.name, &self.url, |remote| {
            Ok(remote
                .refspecs(gix::remote::Direction::Push)
                .iter()
                .map(|s| s.to_ref().to_bstring().to_string())
                .collect())
        })
    }
    #[napi(ts_return_type = "Promise<FetchOutcome>")]
    pub fn fetch(
        &self,
        _env: Env,
        progress: Option<Function<ProgressEvent, ()>>,
    ) -> Result<AsyncTask<FetchTask>> {
        let tsfn = match progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        Ok(AsyncTask::new(FetchTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
            progress: tsfn,
        }))
    }

    /// Perform the git protocol handshake with the remote and return what the server advertised.
    ///
    /// This connects, negotiates the protocol version, and (for V1) collects the advertised
    /// references and capabilities. For V2 the references are not part of the handshake and the
    /// `refs` field will be `null` (use a ref-map operation to obtain them). `direction` defaults
    /// to [`Fetch`](RemoteDirection::Fetch) when omitted.
    ///
    /// Async: the (blocking) network transfer runs on a libuv worker thread.
    #[napi(ts_return_type = "Promise<Handshake>")]
    pub fn handshake(
        &self,
        direction: Option<RemoteDirection>,
    ) -> Result<AsyncTask<HandshakeTask>> {
        Ok(AsyncTask::new(HandshakeTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
            direction: direction
                .map(to_remote_direction)
                .unwrap_or(gix::remote::Direction::Fetch),
        }))
    }

    /// Discover the remote references and how they map onto local tracking branches.
    ///
    /// This performs the equivalent of `git ls-remote` (an `ls-refs` request for protocol V2, or
    /// the inline ref list for V1) and resolves the configured refspecs into concrete mappings.
    /// Unlike [`handshake`](Remote::handshake), this returns the advertised references directly
    /// regardless of the protocol version. `direction` defaults to
    /// [`Fetch`](RemoteDirection::Fetch) when omitted.
    ///
    /// Async: the (blocking) network transfer runs on a libuv worker thread.
    #[napi(ts_return_type = "Promise<RefMap>")]
    pub fn ref_map(&self, direction: Option<RemoteDirection>) -> Result<AsyncTask<RefMapTask>> {
        Ok(AsyncTask::new(RefMapTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
            direction: direction
                .map(to_remote_direction)
                .unwrap_or(gix::remote::Direction::Fetch),
        }))
    }

    /// List references advertised by the remote, mirroring `git ls-remote`.
    ///
    /// With no `refspecs`, every ref the server advertises is returned (the V2 `ls-refs`
    /// command is issued without a `ref-prefix`). When `refspecs` is supplied, the server is
    /// asked to pre-filter by the two-component prefixes derived from those refspecs
    /// (e.g. `refs/heads/` for `refs/heads/*`), the same `RefPrefixes::from_refspecs`
    /// expansion used internally by the protocol's `LsRefsCommand`. `direction` defaults to
    /// [`Fetch`](RemoteDirection::Fetch).
    ///
    /// Unlike [`ref_map`](Self::ref_map), this returns only the flat list of advertised refs
    /// (each a [`HandshakeRef`]) and performs no local tracking-branch mapping.
    ///
    /// Async: the (blocking) network transfer runs on a libuv worker thread.
    #[napi(ts_return_type = "Promise<HandshakeRef[]>")]
    pub fn ls_refs(
        &self,
        refspecs: Option<Vec<String>>,
        direction: Option<RemoteDirection>,
    ) -> Result<AsyncTask<LsRefsTask>> {
        Ok(AsyncTask::new(LsRefsTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
            refspecs,
            direction: direction
                .map(to_remote_direction)
                .unwrap_or(gix::remote::Direction::Fetch),
        }))
    }
}

pub fn build_remote(repo: &Arc<gix::ThreadSafeRepository>, remote: gix::Remote) -> Remote {
    let name = remote
        .name()
        .map(|n| String::from_utf8_lossy(n.as_bstr()).into_owned());
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .or_else(|| remote.url(gix::remote::Direction::Push))
        .map(|u| u.to_string());
    let push_url = remote
        .url(gix::remote::Direction::Push)
        .map(|u| u.to_string());
    Remote {
        repo: Arc::clone(repo),
        name,
        url,
        push_url,
    }
}

/// Summary of a completed [`Remote::fetch`] operation.
#[napi(object)]
pub struct FetchOutcome {
    /// Whether a pack was received (i.e. the remote had new objects for us).
    pub pack_received: bool,
    /// Whether any negotiation with the remote occurred.
    pub negotiated: bool,
}

/// Blocking fetch work, executed on the libuv thread pool by [`AsyncTask`].
pub struct FetchTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
}

impl Task for FetchTask {
    type Output = FetchOutcome;
    type JsValue = FetchOutcome;

    fn compute(&mut self) -> Result<FetchOutcome> {
        let r = thread_local(&self.repo);
        let remote = match &self.name {
            Some(name) => r
                .find_remote(name.as_str())
                .map_err(|e| err("find_remote", e))?,
            None => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| err_str("remote", "anonymous remote has no url"))?;
                r.remote_at(url).map_err(|e| err("remote_at", e))?
            }
        };
        let conn = remote
            .connect(gix::remote::Direction::Fetch)
            .map_err(|e| err("connect", e))?;
        let options = gix::remote::ref_map::Options::default();
        let progress = ProgressCallback::new(self.progress.clone());
        let prepare = conn
            .prepare_fetch(progress.clone(), options)
            .map_err(|e| err("prepare_fetch", e))?;
        let interrupt = std::sync::atomic::AtomicBool::new(false);
        let outcome = prepare
            .receive(progress, &interrupt)
            .map_err(|e| err("receive", e))?;
        let pack_received = matches!(outcome.status, gix::remote::fetch::Status::Change { .. });
        let negotiated = match &outcome.status {
            gix::remote::fetch::Status::Change { .. } => true,
            gix::remote::fetch::Status::NoPackReceived { negotiate, .. } => negotiate.is_some(),
        };
        Ok(FetchOutcome {
            pack_received,
            negotiated,
        })
    }

    fn resolve(&mut self, _env: napi::Env, output: FetchOutcome) -> Result<FetchOutcome> {
        Ok(output)
    }
}

/// The outcome of a protocol [`handshake`](Remote::handshake) with a remote server.
#[napi(object)]
pub struct Handshake {
    /// The protocol version the server responded with (`"V0"`, `"V1"`, or `"V2"`).
    pub server_protocol_version: String,
    /// The advertised references returned as part of a V1 handshake, or `null` for V2
    /// (where references require a separate `ls-refs` request).
    pub refs: Option<Vec<HandshakeRef>>,
    /// The server capabilities, as `(name, optional-value)` pairs.
    pub capabilities: Vec<Capability>,
}

/// A single reference advertised by a remote during the V1 handshake.
#[napi(object)]
pub struct HandshakeRef {
    /// The fully qualified ref name, like `refs/heads/main` or `HEAD`.
    pub name: String,
    /// The object id the ref points to, or `null` for unborn refs.
    pub object_id: Option<String>,
    /// For peeled (annotated-tag) refs, the object the tag points to.
    pub peeled_object_id: Option<String>,
    /// For symbolic refs (e.g. `HEAD`), the ref they point to; `null` otherwise.
    pub target: Option<String>,
    /// The ref kind: `"direct"`, `"peeled"`, `"symbolic"`, or `"unborn"`.
    pub kind: String,
}

/// A single server capability as reported during the handshake.
#[napi(object)]
pub struct Capability {
    pub name: String,
    pub value: Option<String>,
}

/// Blocking handshake work, executed on the libuv thread pool by [`AsyncTask`].
pub struct HandshakeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) direction: gix::remote::Direction,
}

impl Task for HandshakeTask {
    type Output = Handshake;
    type JsValue = Handshake;

    fn compute(&mut self) -> Result<Handshake> {
        let r = thread_local(&self.repo);
        let remote = match &self.name {
            Some(name) => r
                .find_remote(name.as_str())
                .map_err(|e| err("find_remote", e))?,
            None => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| err_str("remote", "anonymous remote has no url"))?;
                r.remote_at(url).map_err(|e| err("remote_at", e))?
            }
        };
        let conn = remote
            .connect(self.direction)
            .map_err(|e| err("connect", e))?;
        let options = gix::remote::ref_map::Options::default();
        let (_refmap, handshake) = conn
            .ref_map(gix::progress::Discard, options)
            .map_err(|e| err("ref_map", e))?;
        Ok(convert_handshake(handshake))
    }

    fn resolve(&mut self, _env: napi::Env, output: Handshake) -> Result<Handshake> {
        Ok(output)
    }
}

fn convert_handshake(h: protocol::Handshake) -> Handshake {
    let server_protocol_version = match h.server_protocol_version {
        protocol::transport::Protocol::V0 => "V0",
        protocol::transport::Protocol::V1 => "V1",
        protocol::transport::Protocol::V2 => "V2",
    }
    .to_string();
    let refs = h
        .refs
        .map(|refs| refs.into_iter().map(convert_ref).collect());
    let capabilities = h
        .capabilities
        .iter()
        .map(|c| Capability {
            name: c.name().to_string(),
            value: c.value().map(|v| v.to_string()),
        })
        .collect();
    Handshake {
        server_protocol_version,
        refs,
        capabilities,
    }
}

fn convert_ref(r: protocol::handshake::Ref) -> HandshakeRef {
    match r {
        protocol::handshake::Ref::Peeled {
            full_ref_name,
            tag,
            object,
        } => HandshakeRef {
            name: full_ref_name.to_string(),
            object_id: Some(object.to_string()),
            peeled_object_id: Some(tag.to_string()),
            target: None,
            kind: "peeled".to_string(),
        },
        protocol::handshake::Ref::Direct {
            full_ref_name,
            object,
        } => HandshakeRef {
            name: full_ref_name.to_string(),
            object_id: Some(object.to_string()),
            peeled_object_id: None,
            target: None,
            kind: "direct".to_string(),
        },
        protocol::handshake::Ref::Symbolic {
            full_ref_name,
            target,
            tag,
            object,
        } => HandshakeRef {
            name: full_ref_name.to_string(),
            object_id: Some(object.to_string()),
            peeled_object_id: tag.map(|t| t.to_string()),
            target: Some(target.to_string()),
            kind: "symbolic".to_string(),
        },
        protocol::handshake::Ref::Unborn {
            full_ref_name,
            target,
        } => HandshakeRef {
            name: full_ref_name.to_string(),
            object_id: None,
            peeled_object_id: None,
            target: Some(target.to_string()),
            kind: "unborn".to_string(),
        },
    }
}

/// The result of discovering the remote's references and how they map to local tracking branches.
///
/// This is what `git ls-remote` reports, plus the refspec resolution performed by gix.
#[napi(object)]
pub struct RefMap {
    /// All references advertised by the remote, regardless of whether they match a refspec.
    pub remote_refs: Vec<HandshakeRef>,
    /// The resolved mappings from remote references to local tracking branches.
    pub mappings: Vec<RefMapping>,
}

/// A single remote reference mapped onto a local destination by the configured refspecs.
#[napi(object)]
pub struct RefMapping {
    /// The matched remote reference, if the mapping source was a named ref (rather than a raw object id).
    pub remote: Option<HandshakeRef>,
    /// The object id this mapping resolves to: the raw id for an object-id source, or the target
    /// object id of the matched remote ref.
    pub remote_object_id: Option<String>,
    /// The local tracking reference to update, if any.
    pub local: Option<String>,
    /// The index into the refspecs (explicit fetch refspecs, or implicit ones added by the remote).
    pub spec_index: u32,
    /// Whether `spec_index` refers to an explicit (`"explicit"`) or implicit (`"implicit"`) refspec.
    pub spec_index_kind: String,
}

/// Blocking ref-map work, executed on the libuv thread pool by [`AsyncTask`].
pub struct RefMapTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) direction: gix::remote::Direction,
}

impl Task for RefMapTask {
    type Output = RefMap;
    type JsValue = RefMap;

    fn compute(&mut self) -> Result<RefMap> {
        let r = thread_local(&self.repo);
        let remote = match &self.name {
            Some(name) => r
                .find_remote(name.as_str())
                .map_err(|e| err("find_remote", e))?,
            None => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| err_str("remote", "anonymous remote has no url"))?;
                r.remote_at(url).map_err(|e| err("remote_at", e))?
            }
        };
        let conn = remote
            .connect(self.direction)
            .map_err(|e| err("connect", e))?;
        let options = gix::remote::ref_map::Options::default();
        let (refmap, _handshake) = conn
            .ref_map(gix::progress::Discard, options)
            .map_err(|e| err("ref_map", e))?;
        Ok(convert_ref_map(refmap))
    }

    fn resolve(&mut self, _env: napi::Env, output: RefMap) -> Result<RefMap> {
        Ok(output)
    }
}

fn convert_ref_map(m: protocol::fetch::RefMap) -> RefMap {
    let remote_refs = m.remote_refs.into_iter().map(convert_ref).collect();
    let mappings = m.mappings.into_iter().map(convert_mapping).collect();
    RefMap {
        remote_refs,
        mappings,
    }
}

fn convert_mapping(map: protocol::fetch::refmap::Mapping) -> RefMapping {
    let (remote, remote_object_id) = match map.remote {
        protocol::fetch::refmap::Source::ObjectId(id) => (None, Some(id.to_string())),
        protocol::fetch::refmap::Source::Ref(r) => {
            let object_id = r.unpack().1.map(|o| o.to_string());
            (Some(convert_ref(r)), object_id)
        }
    };
    let (spec_index, spec_index_kind) = match map.spec_index {
        protocol::fetch::refmap::SpecIndex::ExplicitInRemote(i) => {
            (i as u32, "explicit".to_string())
        }
        protocol::fetch::refmap::SpecIndex::Implicit(i) => (i as u32, "implicit".to_string()),
    };
    RefMapping {
        remote,
        remote_object_id,
        local: map.local.map(|b| b.to_string()),
        spec_index,
        spec_index_kind,
    }
}

/// Blocking ls-refs work, executed on the libuv thread pool by [`AsyncTask`].
pub struct LsRefsTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) refspecs: Option<Vec<String>>,
    pub(crate) direction: gix::remote::Direction,
}

impl Task for LsRefsTask {
    type Output = Vec<HandshakeRef>;
    type JsValue = Vec<HandshakeRef>;

    fn compute(&mut self) -> Result<Vec<HandshakeRef>> {
        let r = thread_local(&self.repo);
        // Resolve the URL, then connect through an *anonymous* remote so the remote's own
        // configured fetch refspecs don't also drive server-side ref-prefix filtering; the
        // caller-supplied refspecs (if any) are the sole source of `ref-prefix` arguments.
        let url = match &self.name {
            Some(name) => {
                let remote = r
                    .find_remote(name.as_str())
                    .map_err(|e| err("find_remote", e))?;
                remote
                    .url(gix::remote::Direction::Fetch)
                    .or_else(|| remote.url(gix::remote::Direction::Push))
                    .map(|u| u.to_string())
                    .ok_or_else(|| err_str("remote", "remote has no url"))?
            }
            None => self
                .url
                .clone()
                .ok_or_else(|| err_str("remote", "anonymous remote has no url"))?,
        };
        let anon = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        let conn = anon
            .connect(self.direction)
            .map_err(|e| err("connect", e))?;
        let specs: Vec<gix::refspec::RefSpec> = match &self.refspecs {
            Some(refspecs) => refspecs
                .iter()
                .map(|spec| {
                    gix::refspec::parse(
                        gix::bstr::BStr::new(spec.as_bytes()),
                        gix::refspec::parse::Operation::Fetch,
                    )
                    .map(|spec| spec.to_owned())
                    .map_err(|e| err("refspec.parse", e))
                })
                .collect::<Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let options = gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: !specs.is_empty(),
            handshake_parameters: Vec::new(),
            extra_refspecs: specs,
        };
        let (refmap, _handshake) = conn
            .ref_map(gix::progress::Discard, options)
            .map_err(|e| err("ref_map", e))?;
        Ok(refmap.remote_refs.into_iter().map(convert_ref).collect())
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<HandshakeRef>) -> Result<Vec<HandshakeRef>> {
        Ok(output)
    }
}

#[napi]
impl Repository {
    /// All configured (named) remotes.
    #[napi]
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let r = thread_local(&self.inner);
        let names = r.remote_names();
        let mut out = Vec::new();
        for name in names.iter() {
            if let Ok(remote) = r.find_remote(&**name) {
                out.push(build_remote(&self.inner, remote));
            }
        }
        Ok(out)
    }

    /// Look up a single named remote.
    #[napi]
    pub fn find_remote(&self, name: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Find the default remote for `direction` as configured, or `null` if none is configured.
    #[napi]
    pub fn find_default_remote(&self, direction: RemoteDirection) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.find_default_remote(to_remote_direction(direction)) {
            Some(Ok(remote)) => Ok(Some(build_remote(&self.inner, remote))),
            Some(Err(e)) => Err(err("find_default_remote", e)),
            None => Ok(None),
        }
    }

    /// Find the remote to fetch from given an optional name/URL (emulates `git fetch <name>`).
    #[napi]
    pub fn find_fetch_remote(&self, name_or_url: Option<String>) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let name_bstr = name_or_url
            .as_deref()
            .map(|s| gix::bstr::BStr::new(s.as_bytes()));
        let remote = r
            .find_fetch_remote(name_bstr)
            .map_err(|e| err("find_fetch_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Find a configured remote by name or URL, returning `null` if it does not exist.
    #[napi]
    pub fn try_find_remote(&self, name_or_url: String) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.try_find_remote(name_or_url.as_str()) {
            Some(Ok(remote)) => Ok(Some(build_remote(&self.inner, remote))),
            Some(Err(e)) => Err(err("try_find_remote", e)),
            None => Ok(None),
        }
    }

    /// Like [`try_find_remote`](Self::try_find_remote) but without applying URL rewrite rules.
    #[napi]
    pub fn try_find_remote_without_url_rewrite(
        &self,
        name_or_url: String,
    ) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.try_find_remote_without_url_rewrite(name_or_url.as_str()) {
            Some(ok) => match ok {
                Ok(remote) => Ok(Some(build_remote(&self.inner, remote))),
                Err(e) => Err(err("try_find_remote", e)),
            },
            None => Ok(None),
        }
    }

    /// The names of all configured remotes.
    #[napi]
    pub fn remote_names(&self) -> Vec<String> {
        let r = thread_local(&self.inner);
        r.remote_names()
            .into_iter()
            .map(|n| n.to_string())
            .collect()
    }

    /// The configured default remote name for `direction`, or `null` if none is configured.
    #[napi]
    pub fn remote_default_name(&self, direction: RemoteDirection) -> Option<String> {
        let r = thread_local(&self.inner);
        r.remote_default_name(to_remote_direction(direction))
            .map(|n| n.to_string())
    }

    /// Create an anonymous remote for `url` (URL rewrite rules are applied).
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create an anonymous remote for `url` without applying URL rewrite rules.
    #[napi]
    pub fn remote_at_without_url_rewrite(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r
            .remote_at_without_url_rewrite(url.as_str())
            .map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create (or overwrite) a named remote `name` pointing at `url` and return it.
    #[napi]
    pub fn create_remote(&self, name: String, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        // Validate the URL the same way gix would when constructing a remote.
        let _ = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;

        let url_key = format!("remote.{name}.url");
        let fetch_key = format!("remote.{name}.fetch");
        let fetch = format!("+refs/heads/*:refs/remotes/{name}/*");

        // Persist to the on-disk local config (mirrors the proven `config_set` path).
        let local_config_path = r.git_dir().join("config");
        let mut file = gix::config::File::from_path_no_includes(
            local_config_path.clone(),
            gix::config::Source::Local,
        )
        .map_err(|e| err("config_load", e))?;
        file.set_raw_value(&url_key, url.as_str())
            .map_err(|e| err("config_set", e))?;
        file.set_raw_value(&fetch_key, fetch.as_str())
            .map_err(|e| err("config_set", e))?;
        std::fs::write(&local_config_path, file.to_bstring().as_slice())
            .map_err(|e| err("config_write", e))?;

        // Read the persisted config back through a fresh open (always reads disk).
        let open_path = r
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| r.git_dir().to_path_buf());
        let fresh = gix::ThreadSafeRepository::open(&open_path)
            .map_err(|e| err("open", e))?
            .to_thread_local();
        let remote = fresh
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }
}
