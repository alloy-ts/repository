//! NAPI bindings for git fetch pack negotiation (`gix_negotiate`).

use napi_derive::napi;

/// Action returned after preparing the commit-graph for negotiation.
#[napi(string_enum)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    NoChange,
    SkipToRefUpdate,
    MustNegotiate,
}

/// Outcome of marking complete and common refs prior to fetch negotiation.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct Action {
    pub kind: ActionKind,
    pub remote_ref_target_known: Option<Vec<bool>>,
}

/// Information about a single round in pack-negotiation.
#[napi(object)]
#[derive(Debug, Clone, Copy)]
pub struct Round {
    pub haves_sent: u32,
    pub in_vain: u32,
    pub haves_to_send: u32,
    pub previous_response_had_at_least_one_in_common: bool,
}

/// State kept between negotiation rounds.
#[napi]
#[derive(Clone, Debug)]
pub struct NegotiationState {
    pub haves_to_send: u32,
    pub seen_ack: bool,
    pub in_vain: u32,
    pub is_stateless: bool,
}

#[napi]
impl NegotiationState {
    #[napi(constructor)]
    pub fn new(connection_is_stateless: bool) -> Self {
        Self {
            haves_to_send: if connection_is_stateless { 32 } else { 16 },
            seen_ack: false,
            in_vain: 0,
            is_stateless: connection_is_stateless,
        }
    }

    #[napi]
    pub fn adjust_window_size(&mut self) {
        if self.seen_ack {
            self.haves_to_send = (self.haves_to_send * 2).min(256);
        } else {
            self.haves_to_send = (self.haves_to_send * 2).min(1024);
        }
    }
}

/// Mark known refs in commit-graph to check if a fetch is needed.
#[napi]
pub fn mark_complete_and_common_ref(
    mapping_count: u32,
    has_unborn_only: bool,
    num_mappings_with_change: u32,
    known_targets: Option<Vec<bool>>,
) -> Action {
    if mapping_count == 0 {
        return Action {
            kind: ActionKind::NoChange,
            remote_ref_target_known: None,
        };
    }

    if has_unborn_only {
        return Action {
            kind: ActionKind::SkipToRefUpdate,
            remote_ref_target_known: None,
        };
    }

    if num_mappings_with_change == 0 {
        return Action {
            kind: ActionKind::NoChange,
            remote_ref_target_known: known_targets,
        };
    }

    let known = known_targets.unwrap_or_else(|| vec![false; mapping_count as usize]);
    Action {
        kind: ActionKind::MustNegotiate,
        remote_ref_target_known: Some(known),
    }
}

/// Predicate result helper checking whether a ref mapping should be ignored.
#[napi]
pub fn make_refmapping_ignore_predicate(include_tags: bool, is_tag_refspec: bool) -> bool {
    include_tags && is_tag_refspec
}

/// Add wants for negotiation based on remote ref target knowledge.
#[napi]
pub fn add_wants(
    remote_ref_target_known: Vec<bool>,
    is_shallow: bool,
    ignored_mask: Option<Vec<bool>>,
) -> Vec<u32> {
    let mut wanted_indices = Vec::new();
    let mask = ignored_mask.unwrap_or_else(|| vec![false; remote_ref_target_known.len()]);

    for (idx, &known) in remote_ref_target_known.iter().enumerate() {
        let is_ignored = mask.get(idx).copied().unwrap_or(false);
        if !is_ignored && (is_shallow || !known) {
            wanted_indices.push(idx as u32);
        }
    }

    wanted_indices
}

/// Simulate or perform one negotiation round.
#[napi]
pub fn one_round(
    state: &mut NegotiationState,
    previous_had_ack: bool,
    haves_available: u32,
) -> Round {
    if previous_had_ack {
        state.seen_ack = true;
        state.in_vain = 0;
    }

    let haves_sent = haves_available.min(state.haves_to_send);
    state.in_vain += haves_sent;

    let round = Round {
        haves_sent,
        in_vain: state.in_vain,
        haves_to_send: state.haves_to_send,
        previous_response_had_at_least_one_in_common: previous_had_ack,
    };

    state.adjust_window_size();
    round
}
