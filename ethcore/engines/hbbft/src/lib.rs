extern crate client_traits;
extern crate common_types;
extern crate engine;
extern crate ethcore_io as io;
extern crate ethcore_miner;
extern crate ethereum_types;
extern crate ethkey;
extern crate hbbft;
extern crate hbbft_testing;
extern crate itertools;
extern crate keccak_hash as hash;
#[macro_use]
extern crate log;
extern crate machine;
extern crate parking_lot;
extern crate rand;
extern crate rustc_hex;
#[macro_use(Serialize, Deserialize)]
extern crate serde;
extern crate ethjson;
extern crate rlp;
extern crate serde_json;

#[cfg(test)]
extern crate ethcore;
#[cfg(test)]
extern crate ethcore_accounts as accounts;
#[cfg(test)]
extern crate proptest;
#[cfg(test)]
extern crate spec;
#[cfg(test)]
extern crate toml;

mod contribution;
mod hbbft_engine;
mod sealing;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;

use std::fmt;

use ethkey::Public;

pub use hbbft_engine::HoneyBadgerBFT;

#[derive(Clone, Copy, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NodeId(pub Public);

impl fmt::Debug for NodeId {
	fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
		write!(f, "{:6}", hex_fmt::HexFmt(&self.0))
	}
}

impl fmt::Display for NodeId {
	fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
		write!(f, "NodeId({})", self.0)
	}
}

#[cfg(test)]
mod tests {
	use crate::test_helpers::{hbbft_client_setup, inject_transaction, HbbftTestData};
	use client_traits::BlockInfo;
	use common_types::ids::BlockId;
	use ethereum_types::H256;
	use ethkey::{KeyPair, Public, Secret};
	use hash::keccak;
	use hbbft::NetworkInfo;
	use hbbft_testing::proptest::{gen_seed, TestRng, TestRngSeed};
	use proptest::{prelude::ProptestConfig, proptest};
	use rand::{Rng, SeedableRng};
	use std::collections::BTreeMap;

	proptest! {
		#![proptest_config(ProptestConfig {
			cases: 1, .. ProptestConfig::default()
		})]

		#[test]
		#[allow(clippy::unnecessary_operation)]
		fn test_miner_transaction_injection(seed in gen_seed()) {
			do_test_miner_transaction_injection(seed)
		}

		#[test]
		#[allow(clippy::unnecessary_operation)]
		fn test_two_clients(seed in gen_seed()) {
			do_test_two_clients(seed)
		}

		#[test]
		#[allow(clippy::unnecessary_operation)]
		fn test_multiple_clients(seed in gen_seed()) {
			do_test_multiple_clients(seed)
		}

		#[test]
		#[allow(clippy::unnecessary_operation)]
		fn test_trigger_at_contribution_threshold(seed in gen_seed()) {
			do_test_trigger_at_contribution_threshold(seed)
		}
	}

	fn generate_nodes<R: Rng>(size: usize, rng: &mut R) -> BTreeMap<Public, HbbftTestData> {
		let keypairs: Vec<KeyPair> = (1..=size)
			.map(|i| {
				let secret = Secret::from(<[u8; 32]>::from(keccak(i.to_string())));
				KeyPair::from_secret(secret).expect("KeyPair generation must succeed")
			})
			.collect();
		let ips_map: BTreeMap<Public, String> = keypairs
			.iter()
			.map(|kp| (*kp.public(), format!("{}", kp.public())))
			.collect();
		let net_infos = NetworkInfo::generate_map(ips_map.keys().cloned(), rng)
			.expect("NetworkInfo generation to always succeed");
		keypairs
			.into_iter()
			.map(|kp| {
				let netinfo = net_infos[kp.public()].clone();
				(*kp.public(), hbbft_client_setup(kp, netinfo, &ips_map))
			})
			.collect()
	}

	// Returns `true` if the node has any unsent messages left.
	fn has_messages(node: &HbbftTestData) -> bool {
		!node.notify.targeted_messages.read().is_empty()
	}

	fn do_test_miner_transaction_injection(seed: TestRngSeed) {
		let mut rng = TestRng::from_seed(seed);
		let test_data = generate_nodes(1, &mut rng)
			.into_iter()
			.nth(0)
			.expect("A NetworkInfo must exist for node 0")
			.1;

		// Verify that we actually start at block 0.
		assert_eq!(test_data.client.chain().best_block_number(), 0);

		// Inject a transaction, with instant sealing a block will be created right away.
		inject_transaction(&test_data.client, &test_data.miner);

		// Expect a new block to be created.
		assert_eq!(test_data.client.chain().best_block_number(), 1);

		// Expect one transaction in the block.
		let block = test_data
			.client
			.block(BlockId::Number(1))
			.expect("Block 1 must exist");
		assert_eq!(block.transactions_count(), 1);
	}

	fn crank_network_single_step(nodes: &BTreeMap<Public, HbbftTestData>) {
		for (from, n) in nodes {
			let mut targeted_messages = n.notify.targeted_messages.write();
			for m in targeted_messages.drain(..) {
				nodes
					.get(&m.1.expect("The Message target node id must be set"))
					.expect("Message target not found in nodes map")
					.client
					.engine()
					.handle_message(&m.0, Some(*from))
					.expect("Message handling to succeed");
			}
		}
	}

	fn crank_network(nodes: &BTreeMap<Public, HbbftTestData>) {
		while nodes.iter().any(|(_, test_data)| has_messages(test_data)) {
			crank_network_single_step(nodes);
		}
	}

	fn test_with_size<R: Rng>(rng: &mut R, size: usize) {
		let nodes = generate_nodes(size, rng);

		for (_, n) in &nodes {
			// Verify that we actually start at block 0.
			assert_eq!(n.client.chain().best_block_number(), 0);
			// Inject transactions to kick off block creation.
			inject_transaction(&n.client, &n.miner);
		}

		// Rudimentary network simulation.
		crank_network(&nodes);

		// All nodes need to have produced a block.
		for (_, n) in &nodes {
			assert_eq!(n.client.chain().best_block_number(), 1);
		}

		// All nodes need to produce the same block with the same hash.
		let mut expected: Option<H256> = None;
		for (_, n) in &nodes {
			match expected {
				None => expected = Some(n.client.chain().best_block_hash()),
				Some(h) => assert_eq!(n.client.chain().best_block_hash(), h),
			}
		}
	}

	fn do_test_two_clients(seed: TestRngSeed) {
		let mut rng = TestRng::from_seed(seed);
		test_with_size(&mut rng, 2);
	}

	fn do_test_multiple_clients(seed: TestRngSeed) {
		let mut rng = TestRng::from_seed(seed);
		let sizes = vec![1, 2, 3, 5, rng.gen_range(6, 10)];
		for size in sizes {
			test_with_size(&mut rng, size);
		}
	}

	fn do_test_trigger_at_contribution_threshold(seed: TestRngSeed) {
		let mut rng = TestRng::from_seed(seed);

		// A network size of 4 allows one adversary.
		// Other nodes should *not* join the epoch if they receive only
		// one contribution, but if 2 or more are received they should!
		let network_size: usize = 4;
		let nodes = generate_nodes(network_size, &mut rng);

		// Get the first node and send a transaction to it.
		let first_node = &nodes.iter().nth(0).unwrap().1;
		let second_node = &nodes.iter().nth(1).unwrap().1;
		inject_transaction(&first_node.client, &first_node.miner);

		// Crank the network until no node has any input
		crank_network(&nodes);

		// We expect no new block being generated in this case!
		assert_eq!(first_node.client.chain().best_block_number(), 0);

		// Get the second node and send a transaction to it.
		inject_transaction(&second_node.client, &second_node.miner);

		// Crank the network until no node has any input
		crank_network(&nodes);

		// This time we do expect a new block has been generated
		assert_eq!(first_node.client.chain().best_block_number(), 1);
	}
}
