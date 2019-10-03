use client_traits::HbbftOptions;
use common_types::transaction::{Action, SignedTransaction, Transaction};
use engine::signer::from_keypair;
use ethcore::client::Client;
use ethcore::miner::{Miner, MinerService};
use ethcore::test_helpers::generate_dummy_client_with_spec;
use ethcore::test_helpers::TestNotify;
use ethereum_types::U256;
use ethkey::{Generator, KeyPair, Public, Random};
use hbbft::crypto::serde_impl::SerdeSecret;
use hbbft::NetworkInfo;
use rustc_hex::FromHex;
use spec::Spec;
use std::collections::BTreeMap;
use std::sync::Arc;

pub fn hbbft_spec() -> Spec {
	Spec::load(
		&::std::env::temp_dir(),
		include_bytes!("../res/honey_badger_bft.json") as &[u8],
	)
	.expect(concat!("Chain spec is invalid."))
}

pub fn hbbft_client() -> std::sync::Arc<ethcore::client::Client> {
	generate_dummy_client_with_spec(hbbft_spec)
}

pub struct HbbftTestData {
	pub client: Arc<Client>,
	pub notify: Arc<TestNotify>,
	pub miner: Arc<Miner>,
}

fn serialize_netinfo(
	net_info: NetworkInfo<Public>,
	ips_map: &BTreeMap<Public, String>,
) -> HbbftOptions {
	let wrapper = SerdeSecret(net_info.secret_key_share().unwrap());
	let hbbft_secret_share = serde_json::to_string(&wrapper).unwrap();

	let hbbft_public_key_set = serde_json::to_string(net_info.public_key_set()).unwrap();

	let hbbft_validator_ip_addresses = serde_json::to_string(ips_map).unwrap();

	HbbftOptions {
		hbbft_secret_share,
		hbbft_public_key_set,
		hbbft_validator_ip_addresses,
	}
}

pub fn hbbft_client_setup(
	keypair: KeyPair,
	net_info: NetworkInfo<Public>,
	ips_map: &BTreeMap<Public, String>,
) -> HbbftTestData {
	assert_eq!(keypair.public(), net_info.our_id());
	let client = hbbft_client();

	// Get miner reference
	let miner = client.miner();
	miner.set_hbbft_options(serialize_netinfo(net_info, ips_map));

	let engine = client.engine();
	// Set the signer *before* registering the client with the engine.
	let signer = from_keypair(keypair);
	engine.set_signer(signer);
	engine.register_client(Arc::downgrade(&client) as _);

	// Register notify object for capturing consensus messages
	let notify = Arc::new(TestNotify::default());
	client.add_notify(notify.clone());

	HbbftTestData {
		client,
		notify,
		miner,
	}
}

pub fn create_transaction() -> SignedTransaction {
	let keypair = Random.generate().unwrap();
	Transaction {
		action: Action::Create,
		value: U256::zero(),
		data: "3331600055".from_hex().unwrap(),
		gas: U256::from(100_000),
		gas_price: U256::zero(),
		nonce: U256::zero(),
	}
	.sign(keypair.secret(), None)
}

pub fn inject_transaction(client: &Arc<Client>, miner: &Arc<Miner>) {
	let transaction = create_transaction();
	miner
		.import_own_transaction(client.as_ref(), transaction.into())
		.unwrap();
}
