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

//! Spec params deserialization.

use std::collections::BTreeMap;
use std::iter;
use ethereum_types::H160;
use uint::{self, Uint};
use hash::{H256, Address};
use bytes::Bytes;

/// Spec params.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct Params {
	/// Account start nonce, defaults to 0.
	pub account_start_nonce: Option<Uint>,
	/// Maximum size of extra data.
	pub maximum_extra_data_size: Uint,
	/// Minimum gas limit.
	pub min_gas_limit: Uint,
	/// The address of a contract that determines the block gas limit.
	pub block_gas_limit_contract: Option<BlockGasLimitContract>,

	/// Network id.
	#[serde(rename = "networkID")]
	pub network_id: Uint,
	/// Chain id.
	#[serde(rename = "chainID")]
	pub chain_id: Option<Uint>,

	/// Name of the main ("eth") subprotocol.
	pub subprotocol_name: Option<String>,

	/// Option fork block number to check.
	pub fork_block: Option<Uint>,
	/// Expected fork block hash.
	#[serde(rename = "forkCanonHash")]
	pub fork_hash: Option<H256>,

	/// See main EthashParams docs.
	pub eip150_transition: Option<Uint>,

	/// See main EthashParams docs.
	pub eip160_transition: Option<Uint>,

	/// See main EthashParams docs.
	pub eip161abc_transition: Option<Uint>,
	/// See main EthashParams docs.
	pub eip161d_transition: Option<Uint>,

	/// See `CommonParams` docs.
	pub eip98_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip155_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub validate_chain_id_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub validate_receipts_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip140_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip210_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip210_contract_address: Option<Address>,
	/// See `CommonParams` docs.
	pub eip210_contract_code: Option<Bytes>,
	/// See `CommonParams` docs.
	pub eip210_contract_gas: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip211_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip145_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip214_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip658_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip1052_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip1283_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip1283_disable_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub eip1014_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub dust_protection_transition: Option<Uint>,
	/// See `CommonParams` docs.
	pub nonce_cap_increment: Option<Uint>,
	/// See `CommonParams` docs.
	pub remove_dust_contracts : Option<bool>,
	/// See `CommonParams` docs.
	#[serde(deserialize_with="uint::validate_non_zero")]
	pub gas_limit_bound_divisor: Uint,
	/// See `CommonParams` docs.
	pub registrar: Option<Address>,
	/// Apply reward flag
	pub apply_reward: Option<bool>,
	/// Node permission contract address.
	pub node_permission_contract: Option<Address>,
	/// See main EthashParams docs.
	pub max_code_size: Option<Uint>,
	/// Maximum size of transaction RLP payload.
	pub max_transaction_size: Option<Uint>,
	/// See main EthashParams docs.
	pub max_code_size_transition: Option<Uint>,
	/// Transaction permission contract address.
	pub transaction_permission_contract: Option<Address>,
	/// Block at which the transaction permission contract should start being used.
	pub transaction_permission_contract_transition: Option<Uint>,
	/// Wasm activation block height, if not activated from start
	pub wasm_activation_transition: Option<Uint>,
	/// KIP4 activiation block height.
	pub kip4_transition: Option<Uint>,
	/// KIP6 activiation block height.
	pub kip6_transition: Option<Uint>,
}

/// A contract that determines block gas limits can be configured either as a single address, or as a map, assigning
/// to each starting block number the contract address that should be used from that block on.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum BlockGasLimitContract {
	/// A single address for a block gas limit contract.
	Single(Address),
	/// A map from block numbers to the contract addresses that should be used for the gas limit after that block.
	Transitions(BTreeMap<Uint, Address>),
}

impl From<BlockGasLimitContract> for BTreeMap<u64, H160> {
	fn from(contract: BlockGasLimitContract) -> Self {
		match contract {
			BlockGasLimitContract::Single(Address(addr)) => iter::once((0, addr)).collect(),
			BlockGasLimitContract::Transitions(transitions) => {
				transitions.into_iter().map(|(Uint(block), Address(addr))| (block.into(), addr)).collect()
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::str::FromStr;
	use serde_json;
	use uint::Uint;
	use ethereum_types::{U256, H160};
	use hash::Address;
	use spec::params::{BlockGasLimitContract, Params};

	#[test]
	fn params_deserialization() {
		let s = r#"{
			"maximumExtraDataSize": "0x20",
			"networkID" : "0x1",
			"chainID" : "0x15",
			"subprotocolName" : "exp",
			"minGasLimit": "0x1388",
			"accountStartNonce": "0x01",
			"gasLimitBoundDivisor": "0x20",
			"maxCodeSize": "0x1000",
			"wasmActivationTransition": "0x1010",
			"blockGasLimitContract": {
				"10": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
				"20": "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
			}
		}"#;

		let deserialized: Params = serde_json::from_str(s).unwrap();
		assert_eq!(deserialized.maximum_extra_data_size, Uint(U256::from(0x20)));
		assert_eq!(deserialized.network_id, Uint(U256::from(0x1)));
		assert_eq!(deserialized.chain_id, Some(Uint(U256::from(0x15))));
		assert_eq!(deserialized.subprotocol_name, Some("exp".to_owned()));
		assert_eq!(deserialized.min_gas_limit, Uint(U256::from(0x1388)));
		assert_eq!(deserialized.account_start_nonce, Some(Uint(U256::from(0x01))));
		assert_eq!(deserialized.gas_limit_bound_divisor, Uint(U256::from(0x20)));
		assert_eq!(deserialized.max_code_size, Some(Uint(U256::from(0x1000))));
		assert_eq!(deserialized.wasm_activation_transition, Some(Uint(U256::from(0x1010))));
	}

	#[test]
	#[should_panic(expected = "a non-zero value")]
	fn test_zero_value_divisor() {
		let s = r#"{
			"maximumExtraDataSize": "0x20",
			"networkID" : "0x1",
			"chainID" : "0x15",
			"subprotocolName" : "exp",
			"minGasLimit": "0x1388",
			"accountStartNonce": "0x01",
			"gasLimitBoundDivisor": "0x0",
			"maxCodeSize": "0x1000"
		}"#;

		let _deserialized: Params = serde_json::from_str(s).unwrap();
	}
}
