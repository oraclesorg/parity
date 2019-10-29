// Copyright 2015-2019 Parity Technologies (UK) Ltd.
// This file is part of Parity Ethereum.

// Parity Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Ethereum.  If not, see <http://www.gnu.org/licenses/>.

//! Authority params deserialization.

use std::collections::BTreeMap;
use hash::Address;
use uint::Uint;
use bytes::Bytes;
use super::{StepDuration, ValidatorSet};

/// Authority params deserialization.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityRoundParams {
	/// Block duration, in seconds.
	pub step_duration: StepDuration,
	/// Valid authorities
	pub validators: ValidatorSet,
	/// Starting step. Determined automatically if not specified.
	/// To be used for testing only.
	pub start_step: Option<Uint>,
	/// Block at which score validation should start.
	pub validate_score_transition: Option<Uint>,
	/// Block from which monotonic steps start.
	pub validate_step_transition: Option<Uint>,
	/// Whether transitions should be immediate.
	pub immediate_transitions: Option<bool>,
	/// Reward per block in wei.
	pub block_reward: Option<Uint>,
	/// Block at which the block reward contract should start being used. This option allows to add
	/// a single block reward contract transition and is compatible with the multiple address option
	/// `block_reward_contract_transitions` below.
	pub block_reward_contract_transition: Option<Uint>,
	/// Block reward contract address (setting the block reward contract overrides the static block
	/// reward definition). This option allows to add a single block reward contract address and is
	/// compatible with the multiple address option `block_reward_contract_transitions` below.
	pub block_reward_contract_address: Option<Address>,
	/// Block reward contract addresses with their associated starting block numbers. Setting the
	/// block reward contract overrides the static block reward definition. If the single block
	/// reward contract address is also present then it is added into the map at the block number
	/// stored in `block_reward_contract_transition` or 0 if that block number is not provided.
	pub block_reward_contract_transitions: Option<BTreeMap<Uint, Address>>,
	/// Block reward code. This overrides the block reward contract address.
	pub block_reward_contract_code: Option<Bytes>,
	/// Block at which maximum uncle count should be considered.
	pub maximum_uncle_count_transition: Option<Uint>,
	/// Maximum number of accepted uncles.
	pub maximum_uncle_count: Option<Uint>,
	/// Block at which empty step messages should start.
	pub empty_steps_transition: Option<Uint>,
	/// Maximum number of accepted empty steps.
	pub maximum_empty_steps: Option<Uint>,
	/// Strict validation of empty steps transition block.
	pub strict_empty_steps_transition: Option<Uint>,
	/// First block for which a 2/3 quorum (instead of 1/2) is required.
	pub two_thirds_majority_transition: Option<Uint>,
	/// If set, enables random number contract integration, and use Proof of
	/// Stake (PoS) consensus.  Otherwise, use Proof of Authority (PoA)
	/// consensus.
	pub randomness_contract_address: Option<Address>,
	/// The block number at which the consensus engine switches from AuRa to AuRa with POSDAO
	/// modifications.
	pub posdao_transition: Option<Uint>,
	/// The addresses of contracts that determine the block gas limit starting from the block number
	/// associated with each of those contracts.
	pub block_gas_limit_contract_transitions: Option<BTreeMap<Uint, Address>>,
}

/// Authority engine deserialization.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityRound {
	/// Authority Round parameters.
	pub params: AuthorityRoundParams,
}

#[cfg(test)]
mod tests {
	use std::str::FromStr;

	use ethereum_types::{U256, H160};
	use uint::Uint;
	use serde_json;
	use hash::Address;
	use spec::validator_set::ValidatorSet;
	use spec::authority_round::AuthorityRound;
	use spec::step_duration::StepDuration;

	#[test]
	fn authority_round_deserialization() {
		let s = r#"{
			"params": {
				"stepDuration": "0x02",
				"validators": {
					"list" : ["0xc6d9d2cd449a754c494264e1809c50e34d64562b"]
				},
				"startStep" : 24,
				"validateStepTransition": 150,
				"blockReward": 5000000,
				"maximumUncleCountTransition": 10000000,
				"maximumUncleCount": 5,
				"blockGasLimitContractTransitions": {
					"10": "0x1000000000000000000000000000000000000001",
					"20": "0x2000000000000000000000000000000000000002"
				}
			}
		}"#;

		let deserialized: AuthorityRound = serde_json::from_str(s).unwrap();
		assert_eq!(deserialized.params.step_duration, StepDuration::Single(Uint(U256::from(2))));
		assert_eq!(deserialized.params.validators, ValidatorSet::List(vec![Address(H160::from("0xc6d9d2cd449a754c494264e1809c50e34d64562b"))]));
		assert_eq!(deserialized.params.start_step, Some(Uint(U256::from(24))));
		assert_eq!(deserialized.params.immediate_transitions, None);
		assert_eq!(deserialized.params.maximum_uncle_count_transition, Some(Uint(10_000_000.into())));
		assert_eq!(deserialized.params.maximum_uncle_count, Some(Uint(5.into())));
		let expected_bglc =
			[(Uint(10.into()), Address(H160::from_str("1000000000000000000000000000000000000001").unwrap())),
			 (Uint(20.into()), Address(H160::from_str("2000000000000000000000000000000000000002").unwrap()))];
		assert_eq!(deserialized.params.block_gas_limit_contract_transitions,
				   Some(expected_bglc.to_vec().into_iter().collect()));
	}
}
