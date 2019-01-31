//! On-chain randomness generation for authority round
//!
//! This module contains the support code for the on-chain randomness generation used by AuRa. Its
//! core is the finite state machine `RandomnessPhase`, which can be loaded from the blockchain
//! state, then asked to perform potentially necessary transaction afterwards using the `advance()`
//! method.
//!
//! No additional state is kept inside the `RandomnessPhase`, it must be passed in each time.

use ethabi::Hash;
use ethereum_types::{Address, U256};
use hash::keccak;
use rand::Rng;

use super::util::{BoundContract, CallError};
use account_provider::{self, AccountProvider};
use ethstore::ethkey::crypto::{self, ecies};

/// Secret type expected by the contract.
// Note: Conversion from `U256` back into `[u8; 32]` is cumbersome (missing implementations), for
//       this reason we store the raw buffers.
pub type Secret = [u8; 32];

use_contract!(aura_random, "res/contracts/authority_round_random.json");

/// Validated randomness phase state.
///
/// The process of generating random numbers is a simple finite state machine:
///
/// ```text
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
/// ```
///
///
/// Phase transitions are performed by the smart contract and simply queried by the engine.
///
/// A typical case of using `RandomnessPhase` is:
///
/// 1. `RandomnessPhase::load()` the phase from the blockchain data.
/// 2. Call `RandomnessPhase::advance()`.
#[derive(Debug)]
pub enum RandomnessPhase {
	// NOTE: Some states include information already gathered during `load` (e.g. `our_address`,
	//       `round`) for efficiency reasons.
	/// Waiting for the next phase.
	///
	/// This state indicates either the successful revelation in this round or having missed the
	/// window to make a commitment.
	Waiting,
	/// Indicates a commitment is possible, but still missing.
	BeforeCommit { our_address: Address, round: U256 },
	/// Indicates a successful commitment, waiting for the commit phase to end.
	Committed,
	/// Indicates revealing is expected as the next step.
	Reveal { our_address: Address, round: U256 },
}

/// Phase loading error for randomness generation state machine.
///
/// This error usually indicates a bug in either the smart contract, the phase loading function or
/// some state being lost.
///
/// The `LostSecret` and `StaleSecret` will usually result in punishment by the contract or the
/// other validators.
#[derive(Debug)]
pub enum PhaseError {
	/// The smart contract reported a phase as both commitment and reveal phase.
	PhaseConflict,
	/// The smart contract reported that we already revealed something while still being in the
	/// commit phase.
	RevealedInCommit,
	/// Calling a contract to determine the phase failed.
	LoadFailed(CallError),
	/// Failed to schedule a transaction to call a contract.
	TransactionFailed(CallError),
	/// A secret was stored, but it did not match the committed hash.
	StaleSecret,
	/// An error with ECIES encryption.
	Crypto(crypto::Error),
	/// Failed to decrypt stored secret.
	Decrypt(account_provider::SignError),
	/// Failed to retrieve the account's public key.
	AccountPublic(account_provider::SignError),
}

impl RandomnessPhase {
	/// Determine randomness generation state from the contract.
	///
	/// Calls various constant contract functions to determine the precise state that needs to be
	/// handled (that is, the phase and whether or not the current validator still needs to send
	/// commitments or reveal secrets).
	pub fn load(
		contract: &BoundContract,
		our_address: Address,
	) -> Result<RandomnessPhase, PhaseError> {
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
				Ok(RandomnessPhase::BeforeCommit { our_address, round })
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
				Ok(RandomnessPhase::Reveal { our_address, round })
			} else {
				Ok(RandomnessPhase::Waiting)
			}
		}
	}

	/// Advance the randomness state, if necessary.
	///
	/// Creates the transaction necessary to advance the randomness contract's state and schedules
	/// them to run on the `client` inside `contract`.
	///
	/// **Warning**: The `advance()` function should be called only once per block state; otherwise
	///              spurious transactions resulting in punishments might be executed.
	pub fn advance<R: Rng>(
		self,
		contract: &mut BoundContract,
		rng: &mut R,
		accounts: &AccountProvider,
	) -> Result<(), PhaseError> {
		match self {
			RandomnessPhase::Waiting | RandomnessPhase::Committed => (),
			RandomnessPhase::BeforeCommit { round, our_address } => {
				// Check whether a secret has already been committed in this round.
				let committed_hash: Hash = contract
					.call_const(aura_random::functions::get_commit::call(round, our_address))
					.map_err(PhaseError::LoadFailed)?;
				if !committed_hash.is_zero() {
					return Ok(()); // Already committed.
				}

				// Generate a new secret. Compute the secret's hash, and encrypt the secret to ourselves.
				let secret: Secret = rng.gen();
				let secret_hash: Hash = keccak(secret.as_ref());
				let public = accounts.account_public(our_address, None).map_err(PhaseError::AccountPublic)?;
				let cipher = ecies::encrypt(&public, &secret_hash, &secret).map_err(PhaseError::Crypto)?;

				// Schedule the transaction that commits the hash and the encrypted secret.
				contract.push_service_transaction(aura_random::functions::commit_hash::call(secret_hash, cipher))
					.map_err(PhaseError::TransactionFailed)?;
			}
			RandomnessPhase::Reveal { round, our_address } => {
				// Load the hash and encrypted secret that we stored in the commit phase.
				let committed_hash: Hash = contract
					.call_const(aura_random::functions::get_commit::call(round, our_address))
					.map_err(PhaseError::LoadFailed)?;
				let cipher = contract
					.call_const(aura_random::functions::get_cipher::call(round, our_address))
					.map_err(PhaseError::LoadFailed)?;

				// Decrypt the secret and check against the hash.
				let secret_vec = accounts.decrypt(our_address, None, &committed_hash, &cipher)
					.map_err(PhaseError::Decrypt)?;
				let secret = if secret_vec.len() == 32 {
					let mut buf = [0u8; 32];
					buf.copy_from_slice(&secret_vec);
					buf
				} else {
					// This can only happen if there is a bug in the smart contract,
					// or if the entire network goes awry.
					error!(target: "engine", "Decrypted randomness secret has the wrong length.");
					return Err(PhaseError::StaleSecret);
				};
				let secret_hash: Hash = keccak(secret.as_ref());
				if secret_hash != committed_hash {
					error!(target: "engine", "Decrypted randomness secret doesn't agree with the hash.");
					return Err(PhaseError::StaleSecret);
				}

				// We are now sure that we have the correct secret and can reveal it.
				contract.push_service_transaction(aura_random::functions::reveal_secret::call(secret))
					.map_err(PhaseError::TransactionFailed)?;
			}
		}
		Ok(())
	}
}
