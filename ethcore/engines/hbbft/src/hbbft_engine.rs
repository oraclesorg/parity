use crate::contribution::{unix_now_millis, unix_now_secs, Contribution};
use crate::sealing::{self, RlpSig, Sealing};
use crate::NodeId;
use ethcore::block::ExecutedBlock;
use ethcore::client::{BlockId, EngineClient};
use ethcore::engines::signer::EngineSigner;
use ethcore::engines::{
	total_difficulty_fork_choice, Engine, EngineError, EthEngine, ForkChoice, Seal, SealingState,
};
use ethcore::error::{BlockError, Error};
use ethcore::machine::EthereumMachine;
use ethcore::miner::HbbftOptions;
use ethereum_types::H512;
use hbbft::crypto::serde_impl::SerdeSecret;
use hbbft::crypto::{PublicKey, PublicKeySet, SecretKeyShare};
use hbbft::honey_badger::{self, HoneyBadgerBuilder, Step};
use hbbft::{NetworkInfo, Target};
use io::{IoContext, IoHandler, IoService, TimerToken};
use itertools::Itertools;
use parking_lot::RwLock;
use rlp::{self, Decodable, Rlp};
use serde::Deserialize;
use serde_json;
use std::cmp::{max, min};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::sync::{Arc, Weak};
use std::time::Duration;
use types::header::{ExtendedHeader, Header};
use types::transaction::SignedTransaction;
use types::BlockNumber;

type HoneyBadger = honey_badger::HoneyBadger<Contribution, NodeId>;
type Batch = honey_badger::Batch<Contribution, NodeId>;
type TargetedMessage = hbbft::TargetedMessage<Message, NodeId>;
type HbMessage = honey_badger::Message<NodeId>;

/// A message sent between validators that is part of Honey Badger BFT or the block sealing process.
#[derive(Debug, Deserialize, Serialize)]
enum Message {
	/// A Honey Badger BFT message.
	HoneyBadger(usize, HbMessage),
	/// A threshold signature share. The combined signature is used as the block seal.
	Sealing(BlockNumber, sealing::Message),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct HoneyBadgerOptions {
	/// The minimum time duration between blocks, in seconds.
	pub minimum_block_time: u64,
	/// The length of the transaction queue at which block creation should be triggered.
	pub transaction_queue_size_trigger: usize,
	/// Should be true when running unit tests to avoid starting timers.
	pub is_unit_test: Option<bool>,
}

pub struct HoneyBadgerBFT {
	transition_service: IoService<()>,
	client: Arc<RwLock<Option<Weak<dyn EngineClient>>>>,
	signer: RwLock<Option<Box<dyn EngineSigner>>>,
	machine: EthereumMachine,
	network_info: RwLock<Option<NetworkInfo<NodeId>>>,
	honey_badger: RwLock<Option<HoneyBadger>>,
	public_master_key: RwLock<Option<PublicKey>>,
	sealing: RwLock<BTreeMap<BlockNumber, Sealing>>,
	options: HoneyBadgerOptions,
	message_counter: RwLock<usize>,
}

struct TransitionHandler {
	client: Arc<RwLock<Option<Weak<dyn EngineClient>>>>,
	engine: Arc<HoneyBadgerBFT>,
}

const DEFAULT_DURATION: Duration = Duration::from_secs(1);

impl TransitionHandler {
	/// Returns the approximate time duration between the latest block and the minimum block time
	/// or a keep-alive time duration of 1s.
	fn duration_remaining_since_last_block(&self, client: Arc<dyn EngineClient>) -> Duration {
		if let Some(block_header) = client.block_header(BlockId::Latest) {
			// The block timestamp and minimum block time are specified in seconds.
			let next_block_time =
				(block_header.timestamp() + self.engine.options.minimum_block_time) as u128 * 1000;

			// We get the current time in milliseconds to calculate the exact timer duration.
			let now = unix_now_millis();

			if now >= next_block_time {
				// If the current time is already past the minimum time for the next block,
				// just return the 1s keep alive interval.
				DEFAULT_DURATION
			} else {
				// Otherwise wait the exact number of milliseconds needed for the
				// now >= next_block_time condition to be true.
				// Since we know that "now" is smaller than "next_block_time" at this point
				// we also know that "next_block_time - now" will always be a positive number.
				match u64::try_from(next_block_time - now) {
					Ok(value) => Duration::from_millis(value),
					_ => {
						error!(target: "consensus", "Could not convert duration to next block to u64");
						DEFAULT_DURATION
					}
				}
			}
		} else {
			error!(target: "consensus", "Latest Block Header could not be obtained!");
			DEFAULT_DURATION
		}
	}
}

// Arbitrary identifier for the timer we register with the event handler.
const ENGINE_TIMEOUT_TOKEN: TimerToken = 1;

impl IoHandler<()> for TransitionHandler {
	fn initialize(&self, io: &IoContext<()>) {
		// Start the event loop with an arbitrary timer
		io.register_timer_once(ENGINE_TIMEOUT_TOKEN, DEFAULT_DURATION)
			.unwrap_or_else(
				|e| warn!(target: "consensus", "Failed to start consensus timer: {}.", e),
			)
	}

	fn timeout(&self, io: &IoContext<()>, timer: TimerToken) {
		if timer == ENGINE_TIMEOUT_TOKEN {
			trace!(target: "consensus", "Honey Badger IoHandler timeout called");
			// The block may be complete, but not have been ready to seal - trigger a new seal attempt.
			// TODO: In theory, that should not happen. The seal is ready exactly when the sealing entry is `Complete`.
			if let Some(ref weak) = *self.client.read() {
				if let Some(c) = weak.upgrade() {
					c.update_sealing();
				}
			}

			// Transactions may have been submitted during creation of the last block, trigger the
			// creation of a new block if the transaction threshold has been reached.
			self.engine.on_transactions_imported();

			// The client may not be registered yet on startup, we set the default duration.
			let mut timer_duration = DEFAULT_DURATION;
			if let Some(ref weak) = *self.client.read() {
				if let Some(c) = weak.upgrade() {
					timer_duration = self.duration_remaining_since_last_block(c);
					// The duration should be at least 1ms and at most self.engine.options.minimum_block_time
					timer_duration = max(timer_duration, Duration::from_millis(1));
					timer_duration = min(
						timer_duration,
						Duration::from_secs(self.engine.options.minimum_block_time),
					);
				}
			}

			io.register_timer_once(ENGINE_TIMEOUT_TOKEN, timer_duration)
				.unwrap_or_else(
					|e| warn!(target: "consensus", "Failed to restart consensus step timer: {}.", e),
				);
		}
	}
}

impl HoneyBadgerBFT {
	pub fn new(
		params: &serde_json::Value,
		machine: EthereumMachine,
	) -> Result<Arc<dyn EthEngine>, Box<Error>> {
		let options = match HoneyBadgerOptions::deserialize(params) {
			Ok(options) => options,
			Err(e) => panic!("HoneyBadgerBFTParams: Invalid chain spec options\n{}", e),
		};
		let engine = Arc::new(HoneyBadgerBFT {
			transition_service: IoService::<()>::start().map_err(|err| Box::new(err.into()))?,
			client: Arc::new(RwLock::new(None)),
			signer: RwLock::new(None),
			machine,
			network_info: RwLock::new(None),
			honey_badger: RwLock::new(None),
			public_master_key: RwLock::new(None),
			sealing: RwLock::new(BTreeMap::new()),
			options,
			message_counter: RwLock::new(0),
		});

		if !engine.options.is_unit_test.unwrap_or(false) {
			let handler = TransitionHandler {
				client: engine.client.clone(),
				engine: engine.clone(),
			};
			engine
				.transition_service
				.register_handler(Arc::new(handler))
				.map_err(|err| Box::new(err.into()))?;
		}

		Ok(engine)
	}

	fn new_network_info(options: HbbftOptions, our_id: NodeId) -> Option<NetworkInfo<NodeId>> {
		let secret_key_share_wrap: SerdeSecret<SecretKeyShare> =
			serde_json::from_str(&options.hbbft_secret_share).ok()?;
		let secret_key_share = secret_key_share_wrap.into_inner();
		let pks: PublicKeySet = serde_json::from_str(&options.hbbft_public_key_set).ok()?;
		let ips: BTreeMap<NodeId, String> =
			serde_json::from_str(&options.hbbft_validator_ip_addresses).ok()?;

		Some(NetworkInfo::new(our_id, secret_key_share, pks, ips.keys()))
	}

	fn new_honey_badger(&self, options: HbbftOptions, our_id: NodeId) -> Option<HoneyBadger> {
		// TODO: Retrieve the information to build a node-specific NetworkInfo
		//       struct from the chain spec and from contracts.
		if let Some(net_info) = HoneyBadgerBFT::new_network_info(options, our_id) {
			let mut builder: HoneyBadgerBuilder<Contribution, _> =
				HoneyBadger::builder(Arc::new(net_info.clone()));
			*self.network_info.write() = Some(net_info);
			return Some(builder.build());
		} else {
			return None;
		}
	}

	fn try_init_honey_badger(&self) {
		let options = if let Some(client) = self.client_arc() {
			// TODO: Retrieve the information to build a node-specific NetworkInfo
			//       struct from the chain spec and from contracts.
			client.hbbft_options().expect("hbbft options have to exist")
		} else {
			return; // No client set yet.
		};
		let pks: PublicKeySet = match serde_json::from_str(&options.hbbft_public_key_set) {
			Ok(pks) => pks,
			Err(err) => {
				error!(target: "engine", "Failed to read public master key from options: {:?}", err);
				return;
			}
		};
		*self.public_master_key.write() = Some(pks.public_key());
		let our_id = if let Some(signer) = self.signer.read().as_ref() {
			NodeId(signer.public().unwrap()) // Can this be `None`?
		} else {
			return; // No engine signer set.
		};
		if let Some(honey_badger) = self.new_honey_badger(options, our_id) {
			*self.honey_badger.write() = Some(honey_badger);
		} else {
			info!(target: "engine", "HoneyBadger algorithm could not be created - running as regular node");
		}
	}

	fn process_output(&self, client: Arc<dyn EngineClient>, output: Vec<Batch>) {
		// TODO: Multiple outputs are possible,
		//       process all outputs, respecting their epoch context.
		if output.len() > 1 {
			error!(target: "consensus", "UNHANDLED EPOCH OUTPUTS!");
		}
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

		if let Some(block) = client.create_pending_block_at(batch_txns, timestamp, batch.epoch) {
			let block_num = block.header.number();
			let hash = block.header.bare_hash();
			trace!(target: "consensus", "Sending signature share of {} for block {}", hash, block_num);
			let step = match self
				.sealing
				.write()
				.entry(block_num)
				.or_insert_with(|| self.new_sealing())
				.sign(hash)
			{
				Ok(step) => step,
				Err(err) => {
					// TODO: Error handling
					error!(target: "consensus", "Error creating signature share for block {}: {:?}", block_num, err);
					return;
				}
			};
			self.process_seal_step(client, step, block_num);
		} else {
			error!(target: "consensus", "Could not create pending block for hbbft epoch {}: ", batch.epoch);
		}
	}

	fn process_hb_message(
		&self,
		msg_idx: usize,
		message: HbMessage,
		sender_id: NodeId,
	) -> Result<(), EngineError> {
		let client = self.client_arc().ok_or(EngineError::RequiresClient)?;
		trace!(target: "consensus", "Received message of idx {}  {:?} from {}", msg_idx, message, sender_id);
		self.honey_badger
			.write()
			.as_mut()
			.map(|honey_badger: &mut HoneyBadger| {
				self.skip_to_current_epoch(&client, honey_badger);
				if let Ok(step) = honey_badger.handle_message(&sender_id, message) {
					self.process_step(client, step);
					self.join_hbbft_epoch(honey_badger);
				} else {
					// TODO: Report consensus step errors
					error!(target: "consensus", "Error on HoneyBadger consensus step");
				}
			})
			.ok_or(EngineError::InvalidEngine)
	}

	fn process_sealing_message(
		&self,
		message: sealing::Message,
		sender_id: NodeId,
		block_num: BlockNumber,
	) -> Result<(), EngineError> {
		let client = self.client_arc().ok_or(EngineError::RequiresClient)?;
		trace!(target: "consensus", "Received sealing message  {:?} from {}", message, sender_id);
		if let Some(latest) = client.block_number(BlockId::Latest) {
			if latest >= block_num {
				return Ok(()); // Message is obsolete.
			}
		}

		trace!(target: "consensus", "Received signature share for block {} from {}", block_num, sender_id);
		let step_result = self
			.sealing
			.write()
			.entry(block_num)
			.or_insert_with(|| self.new_sealing())
			.handle_message(&sender_id, message);
		match step_result {
			Ok(step) => self.process_seal_step(client, step, block_num),
			Err(err) => error!(target: "consensus", "Error on ThresholdSign step: {:?}", err), // TODO: Errors
		}
		Ok(())
	}

	fn dispatch_messages<I>(&self, client: &Arc<dyn EngineClient>, messages: I)
	where
		I: IntoIterator<Item = TargetedMessage>,
	{
		for m in messages {
			let ser =
				serde_json::to_vec(&m.message).expect("Serialization of consensus message failed");
			let opt_net_info = self.network_info.read();
			let net_info = opt_net_info
				.as_ref()
				.expect("Network Info expected to be initialized");
			match m.target {
				Target::Nodes(set) => {
					trace!(target: "consensus", "Dispatching message {:?} to {:?}", m.message, set);
					for node_id in set.into_iter().filter(|p| p != net_info.our_id()) {
						trace!(target: "consensus", "Sending message to {}", node_id.0);
						client.send_consensus_message(ser.clone(), Some(node_id.0));
					}
				}
				Target::AllExcept(set) => {
					trace!(target: "consensus", "Dispatching exclusive message {:?} to all except {:?}", m.message, set);
					for node_id in net_info
						.all_ids()
						.filter(|p| (p != &net_info.our_id() && !set.contains(p)))
					{
						trace!(target: "consensus", "Sending exclusive message to {}", node_id.0);
						client.send_consensus_message(ser.clone(), Some(node_id.0));
					}
				}
			}
		}
	}

	fn process_seal_step(
		&self,
		client: Arc<dyn EngineClient>,
		step: sealing::Step,
		block_num: BlockNumber,
	) {
		let messages = step
			.messages
			.into_iter()
			.map(|msg| msg.map(|m| Message::Sealing(block_num, m)));
		self.dispatch_messages(&client, messages);
		if let Some(sig) = step.output.into_iter().next() {
			trace!(target: "consensus", "Signature for block {} is ready", block_num);
			let state = Sealing::Complete(sig);
			self.sealing.write().insert(block_num, state);
			client.update_sealing();
		}
	}

	fn process_step(&self, client: Arc<dyn EngineClient>, step: Step<Contribution, NodeId>) {
		let mut message_counter = self.message_counter.write();
		let messages = step.messages.into_iter().map(|msg| {
			*message_counter += 1;
			TargetedMessage {
				target: msg.target,
				message: Message::HoneyBadger(*message_counter, msg.message),
			}
		});
		self.dispatch_messages(&client, messages);
		self.process_output(client, step.output);
	}

	fn send_contribution(&self, client: Arc<dyn EngineClient>, honey_badger: &mut HoneyBadger) {
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
				error!(target: "consensus", "Error on HoneyBadger consensus step.");
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

	fn skip_to_current_epoch(
		&self,
		client: &Arc<dyn EngineClient>,
		honey_badger: &mut HoneyBadger,
	) {
		if let Some(parent_block_number) = client.block_number(BlockId::Latest) {
			honey_badger.skip_to_epoch(parent_block_number + 1);
		} else {
			error!(target: "consensus", "The current chain latest block number could not be obtained.");
		}
	}

	fn start_hbbft_epoch(&self, client: Arc<dyn EngineClient>) {
		// We silently return if the Honey Badger algorithm is not set, as it is expected in non-validator nodes.
		if let Some(ref mut honey_badger) = *self.honey_badger.write() {
			self.skip_to_current_epoch(&client, honey_badger);
			if !honey_badger.has_input() {
				self.send_contribution(client, honey_badger);
			}
		}
	}

	fn transaction_queue_and_time_thresholds_reached(
		&self,
		client: &Arc<dyn EngineClient>,
	) -> bool {
		if let Some(block_header) = client.block_header(BlockId::Latest) {
			let target_min_timestamp = block_header.timestamp() + self.options.minimum_block_time;
			let now = unix_now_secs();
			target_min_timestamp <= now
				&& client.queued_transactions().len() >= self.options.transaction_queue_size_trigger
		} else {
			false
		}
	}

	fn new_sealing(&self) -> Sealing {
		let ni_lock = self.network_info.read();
		let netinfo = ni_lock.as_ref().cloned().expect("NetworkInfo not found");
		Sealing::new(netinfo)
	}

	fn client_arc(&self) -> Option<Arc<dyn EngineClient>> {
		self.client.read().as_ref().and_then(Weak::upgrade)
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

	fn verify_block_basic(&self, header: &Header) -> Result<(), Error> {
		if header.seal().len() != 1 {
			return Err(BlockError::InvalidSeal.into());
		}
		let RlpSig(sig) = rlp::decode(header.seal().first().ok_or(BlockError::InvalidSeal)?)?;
		if self
			.public_master_key
			.read()
			.expect("Missing public master key")
			.verify(&sig, header.bare_hash())
		{
			Ok(())
		} else {
			Err(BlockError::InvalidSeal.into())
		}
	}

	fn fork_choice(&self, new: &ExtendedHeader, current: &ExtendedHeader) -> ForkChoice {
		total_difficulty_fork_choice(new, current)
	}

	fn register_client(&self, client: Weak<dyn EngineClient>) {
		*self.client.write() = Some(client.clone());
		self.try_init_honey_badger();
	}

	fn set_signer(&self, signer: Box<dyn EngineSigner>) {
		*self.signer.write() = Some(signer);
		self.try_init_honey_badger();
	}

	fn clear_signer(&self) {
		*self.signer.write() = Default::default();
	}

	fn sealing_state(&self) -> SealingState {
		// Purge obsolete sealing processes.
		let client = match self.client_arc() {
			None => return SealingState::NotReady,
			Some(client) => client,
		};
		let next_block = match client.block_number(BlockId::Latest) {
			None => return SealingState::NotReady,
			Some(block_num) => block_num + 1,
		};
		let mut sealing = self.sealing.write();
		*sealing = sealing.split_off(&next_block);

		// We are ready to seal if we have a valid signature for the next block.
		if let Some(next_seal) = sealing.get(&next_block) {
			if next_seal.signature().is_some() {
				return SealingState::Ready;
			}
		}
		SealingState::NotReady
	}

	fn on_transactions_imported(&self) {
		if let Some(client) = self.client_arc() {
			if self.transaction_queue_and_time_thresholds_reached(&client) {
				self.start_hbbft_epoch(client);
			}
		}
	}

	fn handle_message(&self, message: &[u8], node_id: Option<H512>) -> Result<(), EngineError> {
		let node_id = NodeId(node_id.ok_or(EngineError::UnexpectedMessage)?);
		match serde_json::from_slice(message) {
			Ok(Message::HoneyBadger(msg_idx, hb_msg)) => {
				self.process_hb_message(msg_idx, hb_msg, node_id)
			}
			Ok(Message::Sealing(block_num, seal_msg)) => {
				self.process_sealing_message(seal_msg, node_id, block_num)
			}
			Err(_) => Err(EngineError::MalformedMessage(
				"Serde decoding failed.".into(),
			)),
		}
	}

	fn on_prepare_block(&self, _block: &ExecutedBlock) -> Result<Vec<SignedTransaction>, Error> {
		// TODO: inject random number transactions
		Ok(Vec::new())
	}

	fn seal_fields(&self, _header: &Header) -> usize {
		1
	}

	fn generate_seal(&self, block: &ExecutedBlock, _parent: &Header) -> Seal {
		let block_num = block.header.number();
		let sealing = self.sealing.read();
		let sig = match sealing.get(&block_num).and_then(Sealing::signature) {
			None => return Seal::None,
			Some(sig) => sig,
		};
		if !self
			.public_master_key
			.read()
			.expect("Missing public master key")
			.verify(sig, block.header.bare_hash())
		{
			error!(target: "consensus", "Threshold signature does not match new block.");
			return Seal::None;
		}
		trace!(target: "consensus", "Returning seal for block {}.", block_num);
		Seal::Regular(vec![rlp::encode(&RlpSig(sig))])
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
