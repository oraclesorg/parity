
//! An hbbft <-> Parity link which relays events and acts as an intermediary.

#![allow(dead_code, unused_imports, unused_variables, unused_mut, missing_docs)]

use std::collections::HashMap;
use std::iter;
use std::sync::{Arc, Weak, atomic::{AtomicBool, AtomicIsize, Ordering}};
use std::thread;
use std::time::{Instant, Duration, UNIX_EPOCH};
use std::ops::{Range, BitXorAssign};
// TODO (c0gent): Update rand crate wide.
use rand::{self, OsRng, Rng, distributions::{Sample, Range as RandRange}};
use futures::{
	task, Future, Poll, Stream, Async,
	future::{self, Loop},
	sync::mpsc::Receiver,
	sync::oneshot,
};
use parking_lot::Mutex;
use hydrabadger::{Hydrabadger, Error as HydrabadgerError, Batch, BatchRx, Uid, StateDsct, HydrabadgerWeak,
	EpochRx};
use parity_runtime::Executor;
use tokio::{self, timer::Delay};
use hbbft::HbbftConfig;
use itertools::Itertools;
use rlp::{Decodable, Encodable, Rlp};
use ethstore;
use ethjson::misc::AccountMeta;
use ethkey::{Brain, Generator, Password, Random};
use ethereum_types::{U256, Address};
use header::Header;
use client::{BlockChainClient, Client, ClientConfig, BlockId, ChainInfo, BlockInfo, PrepareOpenBlock,
	ImportSealedBlock, ImportBlock};
use miner::{Miner, MinerService};
use verification::queue::kind::blocks::{Unverified};
use transaction::{Transaction, Action, SignedTransaction};
use block::{OpenBlock, ClosedBlock, IsBlock, LockedBlock, SealedBlock};
use state::{self, State, CleanupMode};
use account_provider::AccountProvider;
use super::laboratory::{Laboratory, Accounts};
use super::key_gen;

type NodeId = Address;

/// Number of random bytes to generate per epoch.
///
/// Currently, we want twenty u32s worth of random data to generated on each epoch.
// TODO (c0gent): Make this configurable somewhere.
const RANDOM_BYTES_PER_EPOCH: usize = 4*20;


/// XOR two slices in-place.
///
/// XORs `src` element-wise onto `dest`, altering `dest` in the process.
fn xor_slices<'a, T>(dest: &'a mut [T], src: &'a [T])
	where T: BitXorAssign<&'a T>,
{
	assert_eq!(dest.len(), src.len(), "::xor_slices: slices must be the same length");
	for (a, b) in dest.iter_mut().zip(src.iter()) {
		*a ^= b;
	}
}


#[derive(Clone, Eq, PartialEq, Debug, Hash, Serialize, Deserialize)]
pub(super) struct Contribution {
	transactions: Vec<Vec<u8>>,
	timestamp: u64,
	/// Random data for on-chain randomness.
	///
	/// The invariant of `random_data.len()` == RANDOM_BYTES_PER_EPOCH **must** hold true.
	random_data: Vec<u8>,
}

// TODO (c0gent): Replace error_chain with failure.
error_chain! {
	types {
		Error, ErrorKind, ErrorResultExt, HbbftDaemonResult;
	}

	errors {
		#[doc = "A tokio runtime start error."]
		RuntimeStart(err: tokio::io::Error) {
			description("Tokio runtime failed to start")
			display("Tokio runtime failed to start: {:?}", err)
		}
		#[doc = "An unhandled hydrabadger error."]
		Hydrabadger(err: HydrabadgerError) {
			description("Unhandled hydrabadger error")
			display("Unhandled hydrabadger error: {:?}", err)
		}
		#[doc = "A hydrabadger batch receiver error."]
		HydrabadgerBatchRxPoll {
			description("Error polling hydrabadger batch receiver")
			display("Error polling hydrabadger batch receiver")
		}
		#[doc = "A hydrabadger epoch receiver error."]
		HydrabadgerEpochRxPoll {
			description("Error polling hydrabadger epoch receiver")
			display("Error polling hydrabadger epoch receiver")
		}
		#[doc = "An ethstore account related error."]
		EthstoreAccountInitNode(err: ethstore::Error) {
			description("ethstore error (node)")
			display("ethstore error (node): {:?}", err)
		}
		#[doc = "An ethstore account related error."]
		EthstoreAccountInitRichie(err: ethstore::Error) {
			description("ethstore error (richie)")
			display("ethstore error (richie): {:?}", err)
		}
		#[doc = "A key generation creation error."]
		KeyGenNew(err: key_gen::Error) {
			description("Error creating key generation instance")
			display("Error creating key generation instance: {:?}", err)
		}
	}
}

/// Methods for use by hbbft.
//
// The purpose of this trait is to keep experimental methods separate and
// organized. TODO (c0gent): Consider this trait's future...
pub trait HbbftClientExt {
	fn a_specialized_method(&self);
	fn change_me_into_something_useful(&self);
	fn import_a_bad_block_and_panic(&self);

	fn set_hbbft_daemon(&self, hbbft_daemon: Arc<HbbftDaemon>);
}

/// Returns the current UNIX Epoch time, in seconds.
fn unix_now_secs() -> u64 {
	UNIX_EPOCH.elapsed().expect("Time not available").as_secs()
}

/// Handles submission of transactions into Hydrabadger.
struct ContributionProposer {
	cfg: HbbftConfig,
	client: Weak<Client>,
	hydrabadger: Hydrabadger<Contribution, Address>,
	block_counter: Arc<AtomicIsize>,
	propose_attempts: usize,
	epoch_rx: EpochRx,
}

impl ContributionProposer {
	fn new(cfg: HbbftConfig, client: Weak<Client>, hydrabadger: Hydrabadger<Contribution, Address>,
		block_counter: Arc<AtomicIsize>, epoch_rx: EpochRx) -> ContributionProposer
	{
		ContributionProposer { cfg, client, hydrabadger, block_counter, propose_attempts: 0, epoch_rx }
	}

	/// Returns the current number of transactions needed before a
	/// contribution is pushed.
	fn next_batch_threshold(&mut self) -> usize {
		let threshold = 1 << (self.cfg.contribution_size_max_log2.saturating_sub(self.propose_attempts));
		self.propose_attempts += 1;
		threshold
	}

	/// Inputs pending transactions as this node's contribution for the next batch into Honey Badger.
	///
	/// Called every `CONTRIBUTION_PUSH_DELAY_MS`.
	fn propose_contribution(&mut self) {
		let client = match self.client.upgrade() {
			Some(client) => client,
			None => return,
		};

		// Select new transactions and propose them for the next block.
		let batch_threshold = self.next_batch_threshold();

		let validator_count = self.hydrabadger.peers().count_validators() + 1;
		let pending = client.miner().pending_transactions_from_queue(&*client,
			1 << self.cfg.contribution_size_max_log2);

		if !self.hydrabadger.is_validator()
			|| validator_count < 2
			|| (pending.len() < batch_threshold
				&& !self.hydrabadger.state().dhb().map(|dhb| dhb.should_propose()).unwrap_or(false))
		{
			// Postpone the next epoch.
			return;
		}

		match self.epoch_rx.poll() {
			Ok(Async::Ready(Some(epoch))) => {
				debug!("CONTRIBUTION_PROPOSER: epoch {} has begun.", epoch);
			}
			Ok(Async::Ready(None)) => {
				info!("CONTRIBUTION_PROPOSER: Hydrabadger epoch tx has dropped.",);
				return;
			}
			Ok(Async::NotReady) => {
				return;
			}
			Err(err) => panic!("HbbftDaemon: ContributionProposer: Epoch Tx error: {:?}", err),
		}

		// Our contribution size.
		let contrib_size = match pending.len() / validator_count {
			0 => 16,
			s => s + 16,
		};

		let mut rng = rand::thread_rng();
		let txns = if pending.len() <= contrib_size {
			pending
		} else {
			debug!("Limiting proposal to {} transactions.", contrib_size);
			rand::seq::sample_slice(&mut rng, &pending, contrib_size)
		};
		info!("ContributionProposer is proposing {} transactions to hydrabadger.", txns.len());
		let ser_txns: Vec<_> = txns.into_iter().map(|txn| txn.signed().rlp_bytes()).collect();
		let contribution = Contribution {
			transactions: ser_txns,
			timestamp: unix_now_secs(),
			random_data: rng.gen_iter().take(RANDOM_BYTES_PER_EPOCH).collect(),
		};
		info!("Proposing {} transactions (after {} attempts).", contribution.transactions.len(),
			self.propose_attempts);

		self.hydrabadger.propose_user_contribution(contribution)
			.expect("TODO: Add transactions back to miner txn queue");

		// Reset push attempts counter:
		self.propose_attempts = 0;
	}

	/// Consumes this `ContributionProposer` and returns a `LoopFn` which calls
	/// `::propose_contribution` every `CONTRIBUTION_PUSH_DELAY_MS`.
	fn into_loop(self) -> impl Future<Item = (), Error = ()> + Send {
		future::loop_fn(self, |mut cp| {
			cp.propose_contribution();

			// This can be adjusted dynamically if needed:
			let loop_delay = cp.cfg.contribution_delay_ms;

			Delay::new(Instant::now() + Duration::from_millis(loop_delay))
				.map(|_| Loop::Continue(cp))
				.map_err(|err| panic!("{:?}", err))
		})
	}
}

// impl Future for ContributionProposer {
// 	type Item = ();
// 	type Error = Error;

// 	/// Polls the batch receiver until the hydrabadger handler batch
// 	/// transmitter (e.g. handler) is dropped.
// 	fn poll(&mut self) -> Poll<(), Error> {
// 		match self.epoch_rx.poll() {
// 			Ok(Async::Ready(Some(epoch))) => {
// 				// TODO: Add delay.
// 				info!("####### CONTRIBUTION_PROPOSER: epoch {} has begun.", epoch);
// 				self.propose_contribution(epoch);
// 			}
// 			Ok(Async::Ready(None)) => {
// 				return Ok(Async::Ready(()));
// 			}
// 			Ok(Async::NotReady) => {}
// 			Err(()) => return Err(ErrorKind::HydrabadgerEpochRxPoll.into()),
// 		}
// 		Ok(Async::NotReady)
// 	}
// }


/// Handles honey badger batch outputs.
//
// TODO: Create a transaction queue semaphore to allow/disallow transactions
// from being streamed into hydrabadger and manipulate its state from here.
struct BatchHandler {
	batch_rx: BatchRx<Contribution, Address>,
	client: Weak<Client>,
	hydrabadger: Hydrabadger<Contribution, Address>,
	block_counter: Arc<AtomicIsize>,
}

impl BatchHandler {
	fn new(batch_rx: BatchRx<Contribution, Address>, client: Weak<Client>, hydrabadger: Hydrabadger<Contribution, Address>,
		block_counter: Arc<AtomicIsize>) -> BatchHandler
	{
		BatchHandler { batch_rx, client, hydrabadger, block_counter }
	}

	/// Handles a batch of transactions output by the Honey Badger BFT
	/// algorithm.
	fn handle_batch(&mut self, batch: Batch<Contribution, NodeId>) {
		let epoch = batch.epoch();

		let client = match self.client.upgrade() {
			Some(client) => client,
			None => return,
		};

		let timestamps = batch.contributions().map(|(_, c)| c.timestamp).sorted();

		// Reconstruct the random data.
		//
		// Randomness is generated by XOR'ing each contribution's `random_data` part. Since XOR is
		// commutative, the order is irrelevant. All validators will have the same set of
		// contributions at this point, so we are guaranteed to get the same value back each time.
		let mut random_data = [0; RANDOM_BYTES_PER_EPOCH];
		for (_, c) in batch.contributions() {
			xor_slices(&mut random_data, &c.random_data)
		};
		debug!("Produces random data {:?} in epoch {}.", &random_data[..], epoch);

		let batch_txns: Vec<_> = batch.contributions().flat_map(|(_, c)| &c.transactions).filter_map(|ser_txn| {
			// TODO: Report proposers of malformed transactions.
			Decodable::decode(&Rlp::new(ser_txn)).ok()
		}).filter_map(|txn| {
			// TODO: Report proposers of invalidly signed transactions.
			SignedTransaction::new(txn).ok()
		}).collect();

		let miner = client.miner();

		let mut open_block = miner.prepare_new_block(&*client).expect("TODO");

		// TODO: Sync block num with epoch upon startup.
		//
		if open_block.header().number() == epoch {
			// The block's timestamp is the median of the proposed timestamps. This guarantees that at least one correct
			// node's proposal was above it, and at least one was below it.
			let timestamp = open_block.header().timestamp().max(timestamps[timestamps.len() / 2]);
			open_block.set_timestamp(timestamp);
			let min_tx_gas = u64::max_value().into(); // TODO

			let txn_count = batch_txns.len();

			// Create a block from the agreed transactions. Seal it instantly and
			// import it.
			let block = miner.prepare_block_from(open_block, batch_txns, &*client, min_tx_gas).expect("TODO");

			info!("Importing block {} (#{}, epoch: {}, txns: {})",
				block.hash(), block.block().header.number(), epoch, txn_count);

			// TODO (afck/drpete): Replace instant sealing with a threshold signature.
			if !miner.seal_and_import_block_internally(&*client, block) {
				warn!("Failed to seal and import block.");
			}
		} else if open_block.header().number() < epoch {
			error!("Can't produce block: missing parent.");
		} else {
			error!("Block {} already imported.", epoch);
		}

		// Increment the counter used to sync the contribution pusher.
		self.block_counter.store(epoch as isize, Ordering::Release);
	}
}

impl Future for BatchHandler {
	type Item = ();
	type Error = Error;

	/// Polls the batch receiver until the hydrabadger handler batch
	/// transmitter (e.g. handler) is dropped.
	fn poll(&mut self) -> Poll<(), Error> {
		const BATCHES_PER_TICK: usize = 3;

		for i in 0..BATCHES_PER_TICK {
			match self.batch_rx.poll() {
				Ok(Async::Ready(Some(batch))) => {
					self.handle_batch(batch);

					// Exceeded max batches per tick, schedule notification:
					if i + 1 == BATCHES_PER_TICK {
						task::current().notify();
					}
				}
				Ok(Async::Ready(None)) => {
					// Batch handler has dropped.
					return Ok(Async::Ready(()));
				}
				Ok(Async::NotReady) => {}
				Err(()) => return Err(ErrorKind::HydrabadgerBatchRxPoll.into()),
			};
		}

		Ok(Async::NotReady)
	}
}

/// An hbbft <-> Parity link which relays events and acts as an intermediary.
pub struct HbbftDaemon {
	// Unused:
	client: Weak<Client>,
	hydrabadger: HydrabadgerWeak<Contribution, Address>,
}

impl HbbftDaemon {
	/// Returns a new `HbbftDaemon`.
	pub fn new(
		client: Arc<Client>,
		cfg: &HbbftConfig,
		account_provider: Arc<AccountProvider>,
		executor: &Executor,
	) -> Result<HbbftDaemon, Error> {
		let mut hdb_config = cfg.to_hydrabadger();

		// Set our starting epoch equal to the best block number in the chain:
		hdb_config.start_epoch =  client.chain_info().best_block_number;

		// Set up local accounts (used temporarily):
		//
		// TODO: Use (and require) engine signer account.
		let accounts = Accounts::new(&*account_provider, &*client, &cfg.bind_address.to_string(),
			cfg.txn_gen_count, 5)?;

		// Spawn Hydrabadger node:
		let hydrabadger = Hydrabadger::<Contribution, Address>::new(cfg.bind_address, hdb_config,
			accounts.signer_account().address);
		let hdb_peers = cfg.remote_addresses.clone();
		executor.spawn(hydrabadger.clone().node(Some(hdb_peers), None));

		// Used by laboratory:
		let block_counter = Arc::new(AtomicIsize::new(-1));

		let epoch_rx = hydrabadger.register_epoch_listener();

		// Spawn contribution pusher:
		executor.spawn(ContributionProposer::new(
			cfg.clone(),
			Arc::downgrade(&client),
			hydrabadger.clone(),
			block_counter.clone(),
			epoch_rx,
		).into_loop());
		info!("Hbbft contribution pusher has been started.");

		let batch_handler = BatchHandler::new(
			hydrabadger.batch_rx()
				.expect("The Hydrabadger batch receiver can not be `None` immediately after creation; qed \
					These proofs are bullshit and prove nothing; qed"),
			Arc::downgrade(&client),
			hydrabadger.clone(),
			block_counter.clone(),
		);

		// Spawn batch handler:
		executor.spawn(batch_handler.map_err(|err| panic!("Unhandled batch handler error: {:?}", err)));
		info!("Hbbft batch handler has been started.");

		// Spawn experimentation loop:
		executor.spawn(Laboratory::new(
			Arc::downgrade(&client),
			hydrabadger.clone(),
			cfg.clone(),
			account_provider,
			accounts,
			block_counter,
		)?.into_loop());

		Ok(HbbftDaemon {
			client: Arc::downgrade(&client),
			hydrabadger: hydrabadger.to_weak(),
		})
	}
}


#[cfg(test)]
mod tests {
	use client::{TestBlockChainClient, EachBlockWith, BlockId, BlockChainClient,
		Nonce, Balance, ChainInfo, BlockInfo, CallContract, TransactionInfo,
		RegistryInfo, ReopenBlock, PrepareOpenBlock, ScheduleInfo, ImportSealedBlock,
		BroadcastProposalBlock, ImportBlock, StateOrBlock, StateInfo, StateClient, Call,
		AccountData, BlockChain as BlockChainTrait, BlockProducer, SealedBlockImporter,
		ClientIoMessage,
	};

	use verification::queue::kind::blocks::{Unverified};
	use rlp::{Rlp, RlpStream, DecoderError};
	use block::{OpenBlock, SealedBlock, ClosedBlock};
	use header::Header;

	use super::xor_slices;



	#[test]
	fn add_transaction() {
		let client = TestBlockChainClient::new();

		let bad_block = Unverified {
			header: Header::default(),
			transactions: vec![],
			uncles: vec![],
			bytes: vec![1, 2, 3],
		};

		client.import_block(bad_block).unwrap();
	}

	#[test]
	fn xor_slices_simple() {
		let mut a = [0b10101010, 0b00001111];
		let b = [0b10010011, 0b00110011];
		let expected = [0b00111001, 0b00111100];
		xor_slices(&mut a, &b);

		assert_eq!(&expected, &a);
	}
}
