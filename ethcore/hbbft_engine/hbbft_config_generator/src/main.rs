extern crate clap;
extern crate ethcore;
extern crate hbbft;
extern crate rand;
extern crate serde;
extern crate toml;

use clap::{App, Arg};
use hbbft::crypto::serde_impl::SerdeSecret;
use hbbft::NetworkInfo;
use serde::Serialize;
use std::fs;
use toml::{map::Map, Value};

fn to_toml_array(vec: Vec<&str>) -> Value {
	Value::Array(vec.iter().map(|s| Value::String(s.to_string())).collect())
}

fn to_toml<N>(net_info: &NetworkInfo<N>, i: usize) -> Value
where
	N: hbbft::NodeIdT + Serialize,
{
	let base_port = 30300i64;
	let base_rpc_port = 8540i64;
	let base_ws_port = 9540i64;

	let mut parity = Map::new();
	parity.insert("chain".into(), Value::String("./spec/spec.json".into()));
	let node_data_path = format!("parity-data/node{}", i);
	parity.insert("base_path".into(), Value::String(node_data_path));

	let mut ui = Map::new();
	ui.insert("disable".into(), Value::Boolean(true));

	let mut network = Map::new();
	network.insert("port".into(), Value::Integer(base_port + i as i64));
	network.insert("nat".into(), Value::String("none".into()));
	network.insert("interface".into(), Value::String("local".into()));
	network.insert(
		"reserved_peers".into(),
		Value::String("parity-data/reserved-peers".into()),
	);

	let mut rpc = Map::new();
	rpc.insert("cors".into(), to_toml_array(vec!["all"]));
	rpc.insert("hosts".into(), to_toml_array(vec!["all"]));
	let apis = to_toml_array(vec![
		"web3",
		"eth",
		"pubsub",
		"net",
		"parity",
		"parity_set",
		"parity_pubsub",
		"personal",
		"traces",
		"rpc",
		"shh",
		"shh_pubsub",
	]);
	rpc.insert("apis".into(), apis);
	rpc.insert("port".into(), Value::Integer(base_rpc_port + i as i64));

	let mut websockets = Map::new();
	websockets.insert("interface".into(), Value::String("all".into()));
	websockets.insert("origins".into(), to_toml_array(vec!["all"]));
	websockets.insert("port".into(), Value::Integer(base_ws_port + i as i64));

	let mut ipc = Map::new();
	ipc.insert("disable".into(), Value::Boolean(true));

	let mut secretstore = Map::new();
	secretstore.insert("disable".into(), Value::Boolean(true));

	let mut ipfs = Map::new();
	ipfs.insert("enable".into(), Value::Boolean(false));

	let mut account = Map::new();
	account.insert(
		"unlock".into(),
		to_toml_array(vec![
			"0xbbcaa8d48289bb1ffcf9808d9aa4b1d215054c78",
			"0x32e4e4c7c5d1cea5db5f9202a9e4d99e56c91a24",
		]),
	);
	account.insert("password".into(), to_toml_array(vec!["config/password"]));

	let mut mining = Map::new();
	// Write Node ID
	let our_id_serialized = serde_json::to_string(&net_info.our_id()).unwrap();
	mining.insert("hbbft_our_id".into(), Value::String(our_id_serialized));

	// Write the Secret Key Share
	let wrapper = SerdeSecret(net_info.secret_key_share().unwrap());
	let sks_serialized = serde_json::to_string(&wrapper).unwrap();
	mining.insert("hbbft_secret_share".into(), Value::String(sks_serialized));

	// Write the Secret Key
	let wrapper = SerdeSecret(net_info.secret_key());
	let sk_serialized = serde_json::to_string(&wrapper).unwrap();
	mining.insert("hbbft_secret_key".into(), Value::String(sk_serialized));

	// Write the Public Key Set
	let pks_serialized = serde_json::to_string(net_info.public_key_set()).unwrap();
	mining.insert("hbbft_public_key_set".into(), Value::String(pks_serialized));

	// Write the Public Keys
	let pk_serialized = serde_json::to_string(net_info.public_key_map()).unwrap();
	mining.insert("hbbft_public_keys".into(), Value::String(pk_serialized));

	mining.insert("force_sealing".into(), Value::Boolean(true));
	mining.insert("min_gas_price".into(), Value::Integer(1000000000));
	mining.insert("reseal_on_txs".into(), Value::String("none".into()));
	mining.insert("extra_data".into(), Value::String("Parity".into()));

	let mut misc = Map::new();
	misc.insert(
		"logging".into(),
		Value::String("engine=trace,miner=trace,reward=trace".into()),
	);

	let mut map = Map::new();
	map.insert("parity".into(), Value::Table(parity));
	map.insert("ui".into(), Value::Table(ui));
	map.insert("network".into(), Value::Table(network));
	map.insert("rpc".into(), Value::Table(rpc));
	map.insert("websockets".into(), Value::Table(websockets));
	map.insert("ipc".into(), Value::Table(ipc));
	map.insert("secretstore".into(), Value::Table(secretstore));
	map.insert("ipfs".into(), Value::Table(ipfs));
	map.insert("account".into(), Value::Table(account));
	map.insert("mining".into(), Value::Table(mining));
	map.insert("misc".into(), Value::Table(misc));
	Value::Table(map)
}

fn main() {
	let matches = App::new("hbbft parity config generator")
		.version("1.0")
		.author("David Forstenlechner <dforsten@gmail.com>")
		.about("Generates n toml files for running a hbbft validator node network")
		.arg(
			Arg::with_name("INPUT")
				.help("The number of config files to generate")
				.required(true)
				.index(1),
		)
		.get_matches();

	let num_nodes: usize = matches
		.value_of("INPUT")
		.expect("Number of nodes input required")
		.parse()
		.expect("Input must be of integer type");
	println!("Number of config files to generate: {}", num_nodes);

	let mut rng = rand::thread_rng();
	let net_infos = NetworkInfo::generate_map(0..num_nodes, &mut rng)
		.expect("NetworkInfo generation expected to succeed");

	for (i, (_, info)) in net_infos.iter().enumerate() {
		// Note: node 0 is a regular full node (not a validator) in the testnet setup, so we start at index 1.
		let file_name = format!("hbbft_validator_{}.toml", i + 1);
		let toml_string =
			toml::to_string(&to_toml(info, i + 1)).expect("TOML string generation should succeed");
		fs::write(file_name, toml_string).expect("Unable to write config file");
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use hbbft::crypto::{PublicKey, PublicKeySet, SecretKey, SecretKeyShare};
	use rand;
	use serde::Deserialize;
	use std::collections::BTreeMap;

	#[derive(Deserialize)]
	struct TomlHbbftOptions {
		pub mining: ethcore::miner::HbbftOptions,
	}

	fn compare<'a, N>(net_info: &NetworkInfo<N>, options: &'a TomlHbbftOptions)
	where
		N: hbbft::NodeIdT + Serialize + Deserialize<'a>,
	{
		// Parse and compare the Public Key Set
		let our_id: N = serde_json::from_str(&options.mining.hbbft_our_id).unwrap();
		assert_eq!(*net_info.our_id(), our_id);

		// Parse and compare the Secret Key Share
		let secret_key_share: SerdeSecret<SecretKeyShare> =
			serde_json::from_str(&options.mining.hbbft_secret_share).unwrap();
		assert_eq!(*net_info.secret_key_share().unwrap(), *secret_key_share);

		// Parse and compare the Secret Key
		let secret_key: SerdeSecret<SecretKey> =
			serde_json::from_str(&options.mining.hbbft_secret_key).unwrap();
		assert_eq!(*net_info.secret_key(), *secret_key);

		// Parse and compare the Public Key Set
		let pks: PublicKeySet = serde_json::from_str(&options.mining.hbbft_public_key_set).unwrap();
		assert_eq!(*net_info.public_key_set(), pks);

		// Parse and compare the Node IDs and Public Keys
		let pk: BTreeMap<N, PublicKey> =
			serde_json::from_str(&options.mining.hbbft_public_keys).unwrap();
		assert_eq!(*net_info.public_key_map(), pk);
	}

	#[test]
	fn test_network_info_serde() {
		let mut rng = rand::thread_rng();
		let net_infos = NetworkInfo::generate_map(0..1usize, &mut rng).unwrap();
		let net_info = net_infos.get(&0).unwrap();

		let toml_string = toml::to_string(&to_toml(&net_info)).unwrap();

		let config: TomlHbbftOptions = toml::from_str(&toml_string).unwrap();
		compare(net_info, &config);
	}
}
