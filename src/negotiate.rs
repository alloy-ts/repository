//! NAPI bindings for Git pack negotiation (`gix_protocol::fetch::negotiate`).
//!
//! Provides types and functions for pack negotiation between client and server:
//! [`NegotiationAction`], [`NegotiationRound`], [`NegotiationState`], and negotiation helpers.

use napi_derive::napi;

/// Action returned after preparing commit-graph for negotiation.
#[napi]
pub enum NegotiationAction {
    /// Remote refs haven't moved; no change required.
    NoChange,
    /// Skip negotiation and pack download; update local refs directly.
    SkipToRefUpdate,
    /// Must perform pack negotiation rounds.
    MustNegotiate,
}

/// Statistics for a single round of pack negotiation.
#[napi(object)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NegotiationRound {
    /// Number of `HAVE` lines sent in this round.
    pub haves_sent: u32,
    /// Number of `HAVE` lines sent across rounds without seeing an acknowledgement.
    pub in_vain: u32,
    /// Target number of `HAVE` lines to send in this round.
    pub haves_to_send: u32,
    /// Whether the server acknowledged at least one common commit in the previous response.
    pub previous_response_had_at_least_one_in_common: bool,
}

/// State maintained across negotiation rounds.
#[napi]
pub struct NegotiationState {
    pub(crate) haves_to_send: u32,
    pub(crate) seen_ack: bool,
    pub(crate) in_vain: u32,
    pub(crate) common_commits: Vec<String>,
}

#[napi]
impl NegotiationState {
    #[napi(constructor)]
    pub fn new(is_stateless: bool) -> Self {
        let initial_window = if is_stateless { 32 } else { 16 };
        Self {
            haves_to_send: initial_window,
            seen_ack: false,
            in_vain: 0,
            common_commits: Vec::new(),
        }
    }

    #[napi(getter)]
    pub fn haves_to_send(&self) -> u32 {
        self.haves_to_send
    }

    #[napi(getter)]
    pub fn seen_ack(&self) -> bool {
        self.seen_ack
    }

    #[napi(getter)]
    pub fn in_vain(&self) -> u32 {
        self.in_vain
    }

    #[napi(getter)]
    pub fn common_commits(&self) -> Vec<String> {
        self.common_commits.clone()
    }

    /// Record a server ACK for a common object ID.
    #[napi]
    pub fn add_common_commit(&mut self, id: String) {
        self.seen_ack = true;
        self.in_vain = 0;
        if !self.common_commits.contains(&id) {
            self.common_commits.push(id);
        }
    }

    /// Advance window size for the next negotiation round.
    #[napi]
    pub fn adjust_window_size(&mut self, is_stateless: bool) {
        if is_stateless {
            self.haves_to_send = (self.haves_to_send * 2).min(16384);
        } else {
            self.haves_to_send = (self.haves_to_send + 16).min(1024);
        }
    }
}

/// Compute negotiation parameters for one round.
#[napi]
pub fn compute_negotiation_round(
    state: &mut NegotiationState,
    haves_available: u32,
    is_stateless: bool,
) -> NegotiationRound {
    let haves_sent = haves_available.min(state.haves_to_send);
    state.in_vain += haves_sent;

    let round = NegotiationRound {
        haves_sent,
        in_vain: state.in_vain,
        haves_to_send: state.haves_to_send,
        previous_response_had_at_least_one_in_common: state.seen_ack,
    };

    state.adjust_window_size(is_stateless);
    round
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_negotiation_state() {
        let mut state = NegotiationState::new(true);
        assert_eq!(state.haves_to_send(), 32);

        state.add_common_commit("abc1234".to_string());
        assert!(state.seen_ack());
        assert_eq!(state.common_commits(), vec!["abc1234".to_string()]);

        let round = compute_negotiation_round(&mut state, 10, true);
        assert_eq!(round.haves_sent, 10);
        assert_eq!(round.previous_response_had_at_least_one_in_common, true);
        assert_eq!(state.haves_to_send(), 64);
    }
}
