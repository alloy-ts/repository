//! NAPI bindings for Git pack negotiation (`gix_negotiate`).

use napi_derive::napi;

/// Action returned after analyzing negotiation state.
#[napi]
pub enum NegotiateAction {
    /// Remote refs match local state; no pack fetch required.
    NoChange,
    /// Objects present locally; skip negotiation and proceed to ref updates.
    SkipToRefUpdate,
    /// Negotiation with server required.
    MustNegotiate,
}

/// Statistics for a single negotiation round.
#[napi(object)]
pub struct NegotiateRound {
    /// Number of HAVEs sent in this round.
    pub haves_sent: u32,
    /// Total HAVEs sent without a matching ACK from server.
    pub in_vain: u32,
    /// Target number of HAVEs to send in this round.
    pub haves_to_send: u32,
    /// Whether the server ACKed at least one commit in the previous round.
    pub previous_response_had_at_least_one_in_common: bool,
}

/// State kept between negotiation rounds.
#[napi]
pub struct NegotiateState {
    haves_to_send: usize,
    in_vain: usize,
    seen_ack: bool,
}

#[napi]
impl NegotiateState {
    /// Create new negotiation state.
    #[napi(constructor)]
    pub fn new(stateless: Option<bool>) -> Self {
        let is_stateless = stateless.unwrap_or(true);
        Self {
            haves_to_send: if is_stateless { 32 } else { 16 },
            in_vain: 0,
            seen_ack: false,
        }
    }

    /// Advance negotiation by one round with simulated HAVEs.
    #[napi]
    pub fn next_round(&mut self, saw_ack: bool) -> NegotiateRound {
        if saw_ack {
            self.in_vain = 0;
            self.seen_ack = true;
        }

        let haves_sent = self.haves_to_send;
        self.in_vain += haves_sent;

        let round = NegotiateRound {
            haves_sent: haves_sent as u32,
            in_vain: self.in_vain as u32,
            haves_to_send: self.haves_to_send as u32,
            previous_response_had_at_least_one_in_common: saw_ack,
        };

        // Window size scaling
        self.haves_to_send = (self.haves_to_send * 2).min(256);

        round
    }

    /// True if negotiation has completed or in-vain threshold reached.
    #[napi(getter)]
    pub fn is_done(&self) -> bool {
        self.seen_ack && self.in_vain >= 256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_negotiate_state_rounds() {
        let mut state = NegotiateState::new(Some(true));
        assert_eq!(state.haves_to_send, 32);

        let round1 = state.next_round(false);
        assert_eq!(round1.haves_sent, 32);
        assert_eq!(round1.in_vain, 32);
        assert!(!round1.previous_response_had_at_least_one_in_common);
        assert!(!state.is_done());

        let round2 = state.next_round(true);
        assert_eq!(round2.haves_sent, 64);
        assert_eq!(round2.in_vain, 64);
        assert!(round2.previous_response_had_at_least_one_in_common);

        // Run until done
        while !state.is_done() {
            state.next_round(false);
        }
        assert!(state.is_done());
    }
}
