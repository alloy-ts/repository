use std::sync::Arc;

use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::remote::Remote;

use gix::OdbHandle;
use gix::RefStore;
use gix_protocol::fetch::Shallow;
use gix_protocol::fetch::negotiate;

/// The decision reached after preparing the commit-graph for negotiation.
///
/// Mirrors `gix_protocol::fetch::negotiate::Action`. Use [`Remote::prepare_negotiation`]
/// to obtain one. `remote_ref_target_known` is parallel to the ref-map mappings:
/// `true` where we already have, locally, the object the remote ref points to.
#[napi(object)]
pub struct NegotiateAction {
    /// One of `"noChange"`, `"skipToRefUpdate"`, or `"mustNegotiate"`.
    pub kind: String,
    /// Present only when `kind == "mustNegotiate"`: per-mapping flag indicating
    /// whether we already hold the advertised object locally.
    pub remote_ref_target_known: Option<Vec<bool>>,
}

impl From<negotiate::Action> for NegotiateAction {
    fn from(a: negotiate::Action) -> Self {
        match a {
            negotiate::Action::NoChange => NegotiateAction {
                kind: "noChange".to_string(),
                remote_ref_target_known: None,
            },
            negotiate::Action::SkipToRefUpdate => NegotiateAction {
                kind: "skipToRefUpdate".to_string(),
                remote_ref_target_known: None,
            },
            negotiate::Action::MustNegotiate {
                remote_ref_target_known,
            } => NegotiateAction {
                kind: "mustNegotiate".to_string(),
                remote_ref_target_known: Some(remote_ref_target_known),
            },
        }
    }
}

/// Per-round statistics for a pack-negotiation round, as produced by `one_round`.
///
/// All fields are plain integers/booleans so the struct is FFI-safe; it is
/// provided now so the wire type exists for the incremental driver (see the
/// module note) and is not yet produced by any bound call.
#[napi(object)]
pub struct NegotiateRound {
    /// The number of `HAVE` lines sent this round.
    pub haves_sent: u32,
    /// Running total, over all rounds, of `HAVE`s sent without an acknowledgement.
    pub in_vain: u32,
    /// How many `HAVE`s the algorithm wants to send next round.
    pub haves_to_send: u32,
    /// Whether the previous response acknowledged at least one common object.
    pub previous_response_had_at_least_one_in_common: bool,
}

#[allow(dead_code)]
impl From<negotiate::Round> for NegotiateRound {
    fn from(r: negotiate::Round) -> Self {
        NegotiateRound {
            haves_sent: r.haves_sent as u32,
            in_vain: r.in_vain as u32,
            haves_to_send: r.haves_to_send as u32,
            previous_response_had_at_least_one_in_common: r.previous_response_had_at_least_one_in_common,
        }
    }
}

/// The number of `HAVE` lines permitted in the next negotiation round.
///
/// Mirrors `gix_negotiate::window_size`: `previous` is the prior window size
/// (omit for the initial window). `stateless` must match the transport — `true`
/// for HTTP, `false` for SSH/git.
#[napi]
pub fn negotiation_window_size(stateless: bool, previous: Option<u32>) -> u32 {
    gix_negotiate::window_size(stateless, previous.map(|p| p as usize)) as u32
}

/// Run the expensive first step of negotiation against a live remote and return the
/// resulting [`NegotiateAction`].
///
/// Connects to this remote, fetches the ref-map, and runs
/// `mark_complete_and_common_ref`. `depth` (when non-zero) narrows the
/// negotiation for a shallow fetch. The result tells the caller whether a fetch
/// is even needed and, when it is, which remote refs' targets we already have
/// locally.
///
/// Async: the (blocking) network transfer and object traversal run on a libuv
/// task thread.
#[napi(ts_return_type = "Promise<NegotiateAction>")]
impl Remote {
    pub fn prepare_negotiation(
        &self,
        depth: Option<u32>,
    ) -> napi::Result<AsyncTask<PrepareNegotiationTask>> {
        Ok(AsyncTask::new(PrepareNegotiationTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
            depth,
        }))
    }
}

/// Blocking negotiation-prepare work, executed on the libuv thread pool by [`AsyncTask`].
pub struct PrepareNegotiationTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) depth: Option<u32>,
}

impl Task for PrepareNegotiationTask {
    type Output = NegotiateAction;
    type JsValue = NegotiateAction;

    fn compute(&mut self) -> napi::Result<NegotiateAction> {
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
        let (ref_map, _handshake) = conn
            .ref_map(gix::progress::Discard, options)
            .map_err(|e| err("ref_map", e))?;

        let shallow = match self.depth.and_then(std::num::NonZeroU32::new) {
            Some(d) => Shallow::DepthAtRemote(d),
            None => Shallow::NoChange,
        };

        // Build a fresh commit graph + negotiator, run `mark_complete_and_common_ref`, and drop them
        // at the end of this call — they are not needed after the action is known. The alternates
        // closure is empty because this repository contributes no alternate object databases.
        let mut graph = gix_negotiate::Graph::new(&r.objects, None);
        let mut negotiator = gix_negotiate::Algorithm::default().into_negotiator();
        let action = negotiate::mark_complete_and_common_ref(
            &r.objects,
            &r.refs,
            || Ok(Vec::<(RefStore, OdbHandle)>::new()),
            negotiator.as_mut(),
            &mut graph,
            &ref_map,
            &shallow,
            |_mapping| false,
        )
        .map_err(|e| err("mark_complete_and_common_ref", e))?;

        Ok(action.into())
    }

    fn resolve(&mut self, _env: napi::Env, output: NegotiateAction) -> napi::Result<NegotiateAction> {
        Ok(output)
    }
}
