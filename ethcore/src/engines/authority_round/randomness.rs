//! On-chain randomness generation for authority round
//!
//! This module contains the support code for the on-chain randomness generation used by AuRa.

use ethereum_types::Address;

use super::util::{BoundContract, CallError};
use client::{BlockId, Client};

use_contract!(aura_random, "res/authority_round_random.json");

/// Validated randomness phase state.
///
/// The process of generating random numbers is a simple finite state machine:
///
///                                                       +
///                                                       |
///                                                       |
///                                                       |
/// +--------------+                              +-------v-------+
/// |              |                              |               |
/// | BeforeCommit <------------------------------+    Waiting    |
/// |              |          enter commit phase  |               |
/// +------+-------+                              +-------^-------+
///        |                                              |
///        |  call                                        |
///        |  `commitHash()`                              |  call
///        |                                              |  `revealSecret`
///        |                                              |
/// +------v-------+                              +-------+-------+
/// |              |                              |               |
/// |  Committed   +------------------------------>    Reveal     |
/// |              |  enter reveal phase          |               |
/// +--------------+                              +---------------+
///
///
/// Phase transitions are performed by the smart contract and simply queried by the engine, while
/// all calls are made directly.
#[derive(Clone, Copy, Debug)]
pub enum RandomnessPhase {
	/// Waiting for the next phase.
	///
	/// This state indicates either the successful revelation in this round or having missed the
	/// window to make a commitment.
	Waiting,
	/// Indicates a commitment is possible, but still missing.
	BeforeCommit,
	/// Indicates a successful commitment, waiting for the commit phase to end.
	Committed,
	/// Indicates revealing is expected as the next step.
	Reveal,
}

/// Phase loading error for randomness generation state machine.
///
/// This error usually indicates a bug in either the smart contract or the phase loading function.
#[derive(Debug)]
pub enum PhaseError {
	/// The smart contract reported a phase as both commitment and reveal phase.
	PhaseConflict,
	/// The smart contract reported that we already revealed something while still being in the
	/// commit phase.
	RevealedInCommit,
	/// Calling a contract to determine the phase failed.
	LoadFailed(CallError),
}

impl RandomnessPhase {
	/// Determine randomness generation state from the contract.
	///
	/// Calls various constant contract functions to determine the precise state that needs to be
	/// handled (that is, the phase and whether or not the current validator still needs to send
	/// commitments or reveal secrets).
	pub fn load(
		client: &Client,
		block_id: BlockId,
		contract_address: Address,
		our_address: Address,
	) -> Result<RandomnessPhase, PhaseError> {
		let contract = BoundContract::bind(client, block_id, contract_address);

		// Determine the current round and which phase we are in.
		let round = contract
			.call_const(aura_random::functions::current_collect_round::call())
			.map_err(PhaseError::LoadFailed)?;
		let is_reveal_phase = contract
			.call_const(aura_random::functions::is_reveal_phase::call())
			.map_err(PhaseError::LoadFailed)?;
		let is_commit_phase = contract
			.call_const(aura_random::functions::is_commit_phase::call())
			.map_err(PhaseError::LoadFailed)?;

		// Ensure we are not committing or revealing twice.
		let committed = contract
			.call_const(aura_random::functions::is_committed::call(
				round,
				our_address,
			))
			.map_err(PhaseError::LoadFailed)?;
		let revealed: bool = contract
			.call_const(aura_random::functions::sent_reveal::call(
				round,
				our_address,
			))
			.map_err(PhaseError::LoadFailed)?;

		// With all the information known, we can determine the actual state we are in.
		if is_reveal_phase && is_commit_phase {
			return Err(PhaseError::PhaseConflict);
		}

		if is_commit_phase {
			if revealed {
				return Err(PhaseError::RevealedInCommit);
			}

			if !committed {
				Ok(RandomnessPhase::BeforeCommit)
			} else {
				Ok(RandomnessPhase::Committed)
			}
		} else {
			if !committed {
				// We apparently entered too late to make a committment, wait until we get a chance
				// again.
				return Ok(RandomnessPhase::Waiting);
			}

			if !revealed {
				Ok(RandomnessPhase::Reveal)
			} else {
				Ok(RandomnessPhase::Waiting)
			}
		}
	}
}
