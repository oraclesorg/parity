use crate::contribution::Contribution;
use ethcore::block::ExecutedBlock;
use ethcore::client::EngineClient;
use ethcore::engines::signer::EngineSigner;
use ethcore::engines::{
	total_difficulty_fork_choice, Engine, EngineError, EthEngine, ForkChoice, Seal, SealingState,
};
use ethcore::error::Error;
use ethcore::machine::EthereumMachine;
use ethcore::miner::HbbftOptions;
use ethereum_types::H512;
use ethkey::Public;
use hbbft::crypto::{serde_impl::SerdeSecret, PublicKey, PublicKeySet, SecretKey, SecretKeyShare};
use hbbft::honey_badger::{HoneyBadgerBuilder, Step};
use hbbft::{NetworkInfo, Target};
use itertools::Itertools;
use parking_lot::RwLock;
use rlp::{Decodable, Rlp};
use serde_json;
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use types::header::{ExtendedHeader, Header};
use types::transaction::SignedTransaction;

type NodeId = Public;
type HoneyBadger = hbbft::honey_badger::HoneyBadger<Contribution, NodeId>;
type Message = hbbft::honey_badger::Message<NodeId>;
type Batch = hbbft::honey_badger::Batch<Contribution, NodeId>;
type TargetedMessage = hbbft::TargetedMessage<Message, NodeId>;

pub struct HoneyBadgerBFT {
	client: Arc<RwLock<Option<Weak<EngineClient>>>>,
	signer: RwLock<Option<Box<EngineSigner>>>,
	machine: EthereumMachine,
	transactions_trigger: usize,
	network_info: RwLock<Option<NetworkInfo<NodeId>>>,
	honey_badger: RwLock<Option<HoneyBadger>>,
}

impl HoneyBadgerBFT {
	pub fn new(
		_params: &serde_json::Value,
		machine: EthereumMachine,
	) -> Result<Arc<EthEngine>, Box<Error>> {
		let engine = Arc::new(HoneyBadgerBFT {
			client: Arc::new(RwLock::new(None)),
			signer: RwLock::new(None),
			machine: machine,
			// TODO: configure through spec params
			transactions_trigger: 1,
			network_info: RwLock::new(None),
			honey_badger: RwLock::new(None),
		});
		Ok(engine)
	}

	fn new_network_info(options: HbbftOptions) -> Option<NetworkInfo<NodeId>> {
		let our_id: NodeId = serde_json::from_str(&options.hbbft_our_id).ok()?;
		let secret_key_share: SerdeSecret<SecretKeyShare> =
			serde_json::from_str(&options.hbbft_secret_share).ok()?;
		let secret_key: SerdeSecret<SecretKey> =
			serde_json::from_str(&options.hbbft_secret_key).ok()?;
		let pks: PublicKeySet = serde_json::from_str(&options.hbbft_public_key_set).ok()?;
		let pk: BTreeMap<NodeId, PublicKey> =
			serde_json::from_str(&options.hbbft_public_keys).ok()?;

		Some(NetworkInfo::new(
			our_id,
			(*secret_key_share).clone(),
			pks,
			(*secret_key).clone(),
			pk,
		))
	}

	fn new_honey_badger(&self) -> Option<HoneyBadger> {
		if let Some(ref weak) = *self.client.read() {
			if let Some(client) = weak.upgrade() {
				// TODO: Retrieve the information to build a node-specific NetworkInfo
				//       struct from the chain spec and from contracts.
				let options = client.hbbft_options().expect("hbbft options have to exist");
				if let Some(net_info) = HoneyBadgerBFT::new_network_info(options) {
					let mut builder: HoneyBadgerBuilder<Contribution, _> =
						HoneyBadger::builder(Arc::new(net_info.clone()));
					*self.network_info.write() = Some(net_info);
					return Some(builder.build());
				} else {
					return None;
				}
			}
		}
		None
	}

	fn process_output(&self, client: Arc<EngineClient>, output: Vec<Batch>) {
		// TODO: Multiple outputs are possible,
		//       process all outputs, respecting their epoch context.
		let batch = match output.first() {
			None => return,
			Some(batch) => batch,
		};

		// Decode and de-duplicate transactions
		let batch_txns: Vec<_> = batch
			.contributions
			.iter()
			.flat_map(|(_, c)| &c.transactions)
			.filter_map(|ser_txn| {
				// TODO: Report proposers of malformed transactions.
				Decodable::decode(&Rlp::new(ser_txn)).ok()
			})
			.unique()
			.filter_map(|txn| {
				// TODO: Report proposers of invalidly signed transactions.
				SignedTransaction::new(txn).ok()
			})
			.collect();

		// We use the median of all contributions' timestamps
		let timestamps = batch
			.contributions
			.iter()
			.map(|(_, c)| c.timestamp)
			.sorted();
		let timestamp = timestamps[timestamps.len() / 2];

		client.create_pending_block(batch_txns, timestamp);
		client.update_sealing();

		// Start a new epoch immediately if the transaction queue
		// is longer than the configured threshold.
		if client.queued_transactions().len() >= self.transactions_trigger {
			self.start_hbbft_epoch(client);
		}
	}

	fn process_message(
		&self,
		message: Message,
		sender_id: NodeId,
	) -> Result<(), EngineError> {
		let client = self
			.client
			.read()
			.as_ref()
			.and_then(|weak| weak.upgrade())
			.ok_or(EngineError::RequiresClient)?;

		self.honey_badger
			.write()
			.as_mut()
			.and_then(|honey_badger: &mut HoneyBadger| {
				if let Ok(step) = honey_badger.handle_message(&sender_id, message) {
					self.process_step(client, step);
					self.join_hbbft_epoch(honey_badger);
				} else {
					// TODO: Report consensus step errors
					error!(target: "engine", "Error on HoneyBadger consensus step");
				}
				Some(())
			})
			.ok_or(EngineError::InvalidEngine)
	}

	fn dispatch_messages(&self, client: &Arc<EngineClient>, messages: Vec<TargetedMessage>) {
		for m in messages {
			if let Ok(ser) = serde_json::to_vec(&m.message) {
				match m.target {
					Target::Node(n) => {
						// for debugging
						// println!("Sending targeted message: {:?}", m.message);
						client.send_consensus_message(ser, n, None);
					}
					Target::All => {
						// for debugging
						// println!("Sending broadcast message: {:?}", m.message);

						if let Some(ref net_info) = *self.network_info.read() {
							for peer_id in net_info.all_ids().filter(|p| p != &net_info.our_id()) {
								client.send_consensus_message(ser.clone(), *peer_id, None);
							}
						} else {
							panic!("Network Info expected to be initialized");
						}
					}
				}
			} else {
				error!(target: "engine", "Serialization of consensus message failed!");
				panic!("Serialization of consensus message failed!");
			}
		}
	}

	fn process_step(&self, client: Arc<EngineClient>, step: Step<Contribution, NodeId>) {
		self.dispatch_messages(&client, step.messages);
		self.process_output(client, step.output);
	}

	fn send_contribution(&self, client: Arc<EngineClient>, honey_badger: &mut HoneyBadger) {
		// TODO: Select a random *subset* of transactions to propose
		let input_contribution = Contribution::new(
			&client
				.queued_transactions()
				.iter()
				.map(|txn| txn.signed().clone())
				.collect(),
		);
		let mut rng = rand::thread_rng();
		let step = honey_badger.propose(&input_contribution, &mut rng);

		match step {
			Ok(step) => {
				self.process_step(client, step);
			}
			_ => {
				// TODO: Report consensus step errors
				error!(target: "engine", "Error on HoneyBadger consensus step.");
			}
		}
	}

	/// Conditionally joins the current hbbft epoch if the number of received
	/// contributions exceeds the maximum number of tolerated faulty nodes.
	fn join_hbbft_epoch(&self, honey_badger: &mut HoneyBadger) {
		if honey_badger.has_input() {
			return;
		}

		if let Some(ref net_info) = *self.network_info.read() {
			if honey_badger.received_proposals() > net_info.num_faulty() {
				if let Some(ref weak) = *self.client.read() {
					if let Some(client) = weak.upgrade() {
						self.send_contribution(client, honey_badger);
					} else {
						panic!("The Client weak reference could not be upgraded.");
					}
				} else {
					panic!("The Client is expected to be set.");
				}
			}
		} else {
			panic!("The Network Info expected to be set.");
		}
	}

	fn start_hbbft_epoch(&self, client: Arc<EngineClient>) {
		if let Some(ref mut honey_badger) = *self.honey_badger.write() {
			if !honey_badger.has_input() {
				self.send_contribution(client, honey_badger);
			}
		} else {
			error!(target: "engine", "Attempt to start an epoch without the honey badger algorithm being set.");
		}
	}
}

impl Engine<EthereumMachine> for HoneyBadgerBFT {
	fn name(&self) -> &str {
		"HoneyBadgerBFT"
	}

	fn machine(&self) -> &EthereumMachine {
		&self.machine
	}

	fn verify_local_seal(&self, _header: &Header) -> Result<(), Error> {
		Ok(())
	}

	fn fork_choice(&self, new: &ExtendedHeader, current: &ExtendedHeader) -> ForkChoice {
		total_difficulty_fork_choice(new, current)
	}

	fn register_client(&self, client: Weak<EngineClient>) {
		*self.client.write() = Some(client.clone());
		if let Some(honey_badger) = self.new_honey_badger() {
			*self.honey_badger.write() = Some(honey_badger);
		} else {
			info!(target: "engine", "HoneyBadger algorithm could not be created - running as regular node");
		}
	}

	fn set_signer(&self, signer: Box<EngineSigner>) {
		*self.signer.write() = Some(signer);
	}

	fn clear_signer(&self) {
		*self.signer.write() = Default::default();
	}

	fn sealing_state(&self) -> SealingState {
		SealingState::Ready
	}

	fn on_transactions_imported(&self) {
		if let Some(ref weak) = *self.client.read() {
			if let Some(client) = weak.upgrade() {
				if client.queued_transactions().len() >= self.transactions_trigger {
					self.start_hbbft_epoch(client);
				}
			}
		}
	}

	fn handle_message(
		&self,
		message: &[u8],
		node_id: Option<H512>,
	) -> Result<(), EngineError> {
		match node_id {
			Some(node_id) => {
				match serde_json::from_slice(message) {
					Ok(decoded_message) => self.process_message(decoded_message, node_id),
					_ => Err(EngineError::UnexpectedMessage),
				}
			},
			None => Err(EngineError::UnexpectedMessage),
		}
	}

	fn on_prepare_block(&self, _block: &ExecutedBlock) -> Result<Vec<SignedTransaction>, Error> {
		// TODO: inject random number transactions
		Ok(Vec::new())
	}

	fn generate_seal(&self, _block: &ExecutedBlock, _parent: &Header) -> Seal {
		// For refactoring/debugging of block creation we seal instantly.
		Seal::Regular(Vec::new())
	}

	fn should_miner_prepare_blocks(&self) -> bool {
		false
	}
}

#[cfg(test)]
mod tests {
	use crate::contribution::Contribution;
	use crate::test_helpers::create_transaction;
	use hbbft::honey_badger::{HoneyBadger, HoneyBadgerBuilder};
	use hbbft::NetworkInfo;
	use rand;
	use std::sync::Arc;
	use types::transaction::SignedTransaction;

	#[test]
	fn test_single_contribution() {
		let mut rng = rand::thread_rng();
		let net_infos = NetworkInfo::generate_map(0..1usize, &mut rng)
			.expect("NetworkInfo generation is expected to always succeed");

		let net_info = net_infos
			.get(&0)
			.expect("A NetworkInfo must exist for node 0");

		let mut builder: HoneyBadgerBuilder<Contribution, _> =
			HoneyBadger::builder(Arc::new(net_info.clone()));

		let mut honey_badger = builder.build();

		let mut pending: Vec<SignedTransaction> = Vec::new();
		pending.push(create_transaction());
		let input_contribution = Contribution::new(&pending);

		let step = honey_badger
			.propose(&input_contribution, &mut rng)
			.expect("Since there is only one validator we expect an immediate result");

		// Assure the contribution returned by HoneyBadger matches the input
		assert_eq!(step.output.len(), 1);
		let out = step.output.first().unwrap();
		assert_eq!(out.epoch, 0);
		assert_eq!(out.contributions.len(), 1);
		assert_eq!(out.contributions.get(&0).unwrap(), &input_contribution);
	}
}
