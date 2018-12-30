use ethereum_types::Address;

use_contract!(foo, "res/contracts/authority_round_random.json");

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
#[derive(Clone, Copy, Debug)]
pub enum PhaseError {
    /// The smart contract reported a phase as both commitment and reveal phase.
    PhaseConflict,
    /// The smart contract reported that we already revealed something while still being in the
    /// commit phase.
    RevealedInCommit,
}

impl RandomnessPhase {
    /// Determine randomness generation state from the contract.
    fn load(contract_address: Address, our_address: Address) -> Result<RandomnessPhase, PhaseError> {
        let is_reveal_phase: bool = unimplemented!(); // call `isRevealPhase()`.
        let is_commit_phase: bool = unimplemented!(); // call `isCommitPhase()`.

        let committed: bool = unimplemented!(); // call `isCommitted()`.
        let revealed: bool = unimplemented!(); // call `sendReveal()`.

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
                return Ok(RandomnessPhase::Waiting)
            }

            if !revealed {
                Ok(RandomnessPhase::Reveal)
            } else {
                Ok(RandomnessPhase::Waiting)
            }
        }
    }
}
