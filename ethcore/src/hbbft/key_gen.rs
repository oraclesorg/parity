//! On-chain synchronous distributed key generation.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::Weak;
use std::collections::{BTreeMap, VecDeque};
use std::cmp;

use futures::sync::mpsc;
use rand;
use serde_cbor;

use hydrabadger::{Hydrabadger, Contribution, NetworkState, Uid, WireMessage};
use hydrabadger::hbbft::{
	crypto::{PublicKey, SecretKey},
	sync_key_gen::{Ack, AckOutcome, Part, PartOutcome, SyncKeyGen, Error as SyncKeyGenError},
};
use ethereum_types::{U256, Address};
use types::{ids::TransactionId, receipt::LocalizedReceipt, filter::Filter, BlockNumber};
use ethkey::Password;
use account_provider::AccountProvider;
use transaction::{Transaction, Action, SignedTransaction, Error as TransactionError};
use client::{BlockChainClient, Client, ClientConfig, BlockId, ChainInfo, BlockInfo, PrepareOpenBlock,
	ImportSealedBlock, ImportBlock, CallContract};
use miner::MinerService;

pub(super) const KEY_GEN_HISTORY_CONTRACT_HEX: &'static str = "608060405234801561001057600080fd5b50610521806100206000396000f300608060405260043610610062576000357c0100000000000000000000000000000000000000000000000000000000900463ffffffff1680630625db39146100675780631c0e378a146100d05780633f8e80dc146100e7578063d207778214610150575b600080fd5b34801561007357600080fd5b506100ce600480360381019080803590602001908201803590602001908080601f016020809104026020016040519081016040528093929190818152602001838380828437820191505050505050919291929050505061017b565b005b3480156100dc57600080fd5b506100e561037c565b005b3480156100f357600080fd5b5061014e600480360381019080803590602001908201803590602001908080601f016020809104026020016040519081016040528093929190818152602001838380828437820191505050505050919291929050505061038f565b005b34801561015c57600080fd5b506101656104ef565b6040518082815260200191505060405180910390f35b60003073ffffffffffffffffffffffffffffffffffffffff1663d20777826040518163ffffffff167c0100000000000000000000000000000000000000000000000000000000028152600401602060405180830381600087803b1580156101e157600080fd5b505af11580156101f5573d6000803e3d6000fd5b505050506040513d602081101561020b57600080fd5b81019080805190602001909291905050505060009050600054813373ffffffffffffffffffffffffffffffffffffffff167f0d1b06290384e9fafa6b46d168dfd792abb701025c0c2e77068fd00881368c64856040518080602001828103825283818151815260200191508051906020019080838360005b8381101561029e578082015181840152602081019050610283565b50505050905090810190601f1680156102cb5780820380516001836020036101000a031916815260200191505b509250505060405180910390a43073ffffffffffffffffffffffffffffffffffffffff1663d20777826040518163ffffffff167c0100000000000000000000000000000000000000000000000000000000028152600401602060405180830381600087803b15801561033c57600080fd5b505af1158015610350573d6000803e3d6000fd5b505050506040513d602081101561036657600080fd5b8101908080519060200190929190505050505050565b6000808154809291906001019190505550565b6000809050600054813373ffffffffffffffffffffffffffffffffffffffff167f273e48fa7559da80314de89c5630a09b8673a98f4e81612decf00cb73345c592856040518080602001828103825283818151815260200191508051906020019080838360005b838110156104115780820151818401526020810190506103f6565b50505050905090810190601f16801561043e5780820380516001836020036101000a031916815260200191505b509250505060405180910390a43073ffffffffffffffffffffffffffffffffffffffff1663d20777826040518163ffffffff167c0100000000000000000000000000000000000000000000000000000000028152600401602060405180830381600087803b1580156104af57600080fd5b505af11580156104c3573d6000803e3d6000fd5b505050506040513d60208110156104d957600080fd5b8101908080519060200190929190505050505050565b600054815600a165627a7a7230582038b1dbd02cabdf8e6d0ca90e7457a2bd99d06ecf7a21a54f499e3c934a169c8b0029";

use_contract!(key_gen_history, "res/contracts/hbbft/key_gen_history.json");

// TODO (c0gent): Replace error_chain with failure.
error_chain! {
	types {
		Error, ErrorKind, ErrorResultExt, HbbftKeyGenResult;
	}

	errors {
		#[doc = "A SyncKeyGen creation error."]
		SyncKeyGenNew(err: SyncKeyGenError) {
			description("Unable to create a SyncKeyGen instance")
			display("Unable to create a SyncKeyGen instance: {:?}", err)
		}
		#[doc = "A CBOR serialization error."]
		CborSerializeError(err: serde_cbor::error::Error) {
			description("CBOR serialization error")
			display("CBOR serialization error: {:?}", err)
		}
		#[doc = "Client has been dropped."]
		ClientDropped {
			description("Client has been dropped")
			display("Client has been dropped")
		}
		#[doc = "TransactionImport."]
		TransactionImport(err: TransactionError) {
			description("Error importing transaction")
			display("Error importing transaction: {:?}", err)
		}
		#[doc = "UnknownAccountNonce."]
		UnknownAccountNonce(addr: Address) {
			description("Unable to determine nonce for account")
			display("Unable to determine nonce for account with address: {}", addr)
		}
	}
}

/// An on-chain synchronous distributed key generation instance.
pub struct KeyGen {
	sync_key_gen: SyncKeyGen<Address>,
	validator_count: usize,
	part_count: usize,
	ack_count: usize,
	signing_address: Address,
	signing_password: Password,
	signing_account_nonce: U256,
	contract_address: Address,
	client: Weak<Client>,
	account_provider: Arc<AccountProvider>,
	last_block_read: BlockNumber,
}

impl KeyGen {
	/// Creates and returns a new `KeyGen` instance.
	pub fn new(
		mut peer_public_keys: BTreeMap<Address, PublicKey>,
		local_nid: &Address,
		local_sk: SecretKey,
		signing_address: Address,
		signing_password: Password,
		contract_address: Address,
		client: Weak<Client>,
		account_provider: Arc<AccountProvider>,
	) -> Result<KeyGen, Error> {
		let local_pk = local_sk.public_key();
		peer_public_keys.insert(*local_nid, local_pk);
		let public_keys = peer_public_keys;

		let validator_count = public_keys.len();
		let threshold = validator_count / 3;

		let mut rng = rand::OsRng::new().expect("Creating OS Rng has failed");

		let (sync_key_gen, opt_part) = SyncKeyGen::new(
			&mut rng,
			*local_nid,
			local_sk,
			public_keys.clone(),
			threshold,
		)
		.map_err(ErrorKind::SyncKeyGenNew)?;

		// TODO: Do something else when we're not a validator:
		let part = opt_part.expect("This node is not a validator");

		let client_arc = client.upgrade().ok_or(ErrorKind::ClientDropped)?;
		let latest_block_num = client_arc.block_number(BlockId::Latest)
			.expect("Unable to determine latest block number");

		let mut kg = KeyGen {
			sync_key_gen,
			validator_count,
			ack_count: 0,
			part_count: 0,
			signing_address,
			signing_password,
			signing_account_nonce: 0.into(),
			contract_address,
			client,
			account_provider,
			last_block_read: latest_block_num,
		};

		kg.write_part(&part, &client_arc)?;

		Ok(kg)
	}

	/// Converts an unsigned `Transaction` to a `SignedTransaction`.
	fn sign_txn(&self, txn: Transaction, chain_id: Option<u64>) -> SignedTransaction {
		let txn_hash = txn.hash(chain_id);
		let sig = self.account_provider.sign(self.signing_address, Some(self.signing_password.clone()), txn_hash)
			.unwrap_or_else(|e| panic!("[hbbft-lab] failed to sign txn: {:?}", e));
		let unverified_txn = txn.with_signature(sig, chain_id);
		SignedTransaction::new(unverified_txn).unwrap()
	}

	/// Pushes a contract call transaction to the queue.
	//
	// TODO: Add a mechanism to ensure each transaction is added to a block.
	// Retry if a transaction is skipped.
	fn call_contract(&mut self, data: Vec<u8>, client: &Client) -> Result<(), Error> {
		let signer_nonce_state = client.state().nonce(&self.signing_address)
			.map_err(|_| ErrorKind::UnknownAccountNonce(self.signing_address))?;
		let signer_nonce = cmp::max(signer_nonce_state, self.signing_account_nonce);

		let txn_signed = self.sign_txn(
			Transaction {
				action: Action::Call(self.contract_address),
				nonce: signer_nonce,
				// gas_price: client.miner().sensible_gas_price(),
				gas_price: 0.into(),
				gas: client.miner().sensible_gas_limit(),
				value: 0.into(),
				data,
			},
			client.signing_chain_id(),
		);

		client.miner().import_claimed_local_transaction(&*client, txn_signed.into(), false)
			.map_err(ErrorKind::TransactionImport)?;
		info!("KEY GENERATION: Transaction imported...");
		self.signing_account_nonce += 1.into();
		Ok(())
	}

	/// Calls the `WritePart` contract function.
	fn write_part(&mut self, part: &Part, client: &Client) -> Result<(), Error> {
		let data = key_gen_history::functions::write_part::encode_input(
			serde_cbor::to_vec(part).map_err(ErrorKind::CborSerializeError)?);
		self.call_contract(data, client)
	}

	/// Calls the `WriteAck` contract function.
	fn write_ack(&mut self, ack: &Ack, client: &Client) -> Result<(), Error> {
		let data = key_gen_history::functions::write_ack::encode_input(
			serde_cbor::to_vec(ack).map_err(ErrorKind::CborSerializeError)?);
		self.call_contract(data, client)
	}

	/// Forwards an `Ack` to a `SyncKeyGen` instance.
	fn handle_ack(&mut self, nid: &Address, ack: Ack) {
		trace!("KEY GENERATION: Handling ack from '{}'...", nid);
		let ack_outcome = self.sync_key_gen
			.handle_ack(nid, ack)
			.expect("Failed to handle Ack.");
		match ack_outcome {
			AckOutcome::Invalid(fault) => error!("Error handling ack: \n{:?}", fault),
			AckOutcome::Valid => self.ack_count += 1,
		}
	}

	/// Handles a received `Part`.
	//
	// TODO: Error handling.
	pub fn handle_part_event(
		&mut self,
		src_nid: &Address,
		part: Part,
	) -> Result<(), Error>{
		trace!("KEY GENERATION: Handling part from '{}'...", src_nid);
		let mut rng = rand::OsRng::new().expect("Creating OS Rng has failed");

		let ack = match self.sync_key_gen.handle_part(&mut rng, src_nid, part) {
			Ok(PartOutcome::Valid(Some(ack))) => ack,
			Ok(PartOutcome::Invalid(faults)) => {
				panic!("Invalid part: {:?}", faults);
			}
			Ok(PartOutcome::Valid(None)) => {
				error!("`SyncKeyGen::handle_part` returned `None`.");
				return Ok(());
			}
			Err(err) => {
				error!("Error handling Part: {:?}", err);
				return Ok(());
			}
		};

		self.part_count += 1;

		trace!("KEY GENERATION: Handling `Ack`.");
		self.handle_ack(src_nid, ack.clone());

		trace!(
			"KEY GENERATION: Part from '{}' acknowledged. Broadcasting ack...",
			src_nid
		);
		let client = self.client.upgrade().ok_or(ErrorKind::ClientDropped)?;
		self.write_ack(&ack, &client)?;

		Ok(())
	}

	/// Handles a received `Ack`.
	pub fn handle_ack_event(
		&mut self,
		src_nid: &Address,
		ack: Ack,
	) -> Result<bool, Error> {
		self.handle_ack(src_nid, ack.clone());

		let node_n = self.validator_count;

		// TODO: Consider modifying this to ~:
		// `sync_key_gen.count_complete() >= num_nodes - num_faulty`.
		if self.sync_key_gen.count_complete() == node_n
			&& self.ack_count >= node_n * node_n
		{
			info!("KEY GENERATION: All acks received and handled.");

			assert!(self.sync_key_gen.is_ready());
			Ok(true)
		} else {
			Ok(false)
		}
	}

	/// Reads the blockchain for events and handles parts and acks
	/// appropriately.
	pub fn handle_events(&mut self) -> Result<(), Error> {
		info!("KEY GENERATION: Handling events...");

		let client = self.client.upgrade().ok_or(ErrorKind::ClientDropped)?;
		let latest_block_num = client.block_number(BlockId::Latest)
			.expect("Unable to determine latest block number");

		let filter = Filter {
			from_block: BlockId::Number(self.last_block_read + 1),
			to_block: BlockId::Number(latest_block_num),
			address: Some(vec![self.contract_address]),
			topics: vec![],
			limit: None,
		};

		let logs = client.logs(filter).unwrap_or_default();

		let parts = logs.iter()
			.filter_map(|entry| {
				let event = key_gen_history::events::part_written::parse_log(
					(entry.topics.clone(), entry.data.clone()).into()
				).ok()?;
				serde_cbor::from_slice(&event.part).ok()
					.map(|part: Part| {
						self.handle_part_event(&event.validator.into(), part.clone())?;
						Ok((part, event.validator.into()))
					})
			})
			.collect::<Result<Vec<(Part, Address)>, Error>>()?;

		info!("KEY GENERATION: Parsed Parts: {:?}", parts);

		let acks = logs.iter()
			.filter_map(|entry| {
				let event = key_gen_history::events::ack_written::parse_log(
					(entry.topics.clone(), entry.data.clone()).into()
				).ok()?;
				serde_cbor::from_slice(&event.ack).ok()
					.map(|ack: Ack| {
						self.handle_ack_event(&event.validator.into(), ack.clone())?;
						Ok((ack, event.validator.into()))
					})
			})
			.collect::<Result<Vec<(Ack, Address)>, Error>>()?;

		info!("KEY GENERATION: Parsed Acks: {:?}", acks);


		self.last_block_read = latest_block_num;
		Ok(())
	}
}