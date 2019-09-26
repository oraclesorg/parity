#[macro_use]
extern crate clap;
extern crate ethcore;
extern crate ethkey;
extern crate hbbft;
extern crate rand;
extern crate rustc_hex;
extern crate serde;
extern crate toml;

use clap::{App, Arg};
use ethkey::{Generator, Public, Random, Secret};
use hbbft::crypto::serde_impl::SerdeSecret;
use hbbft::NetworkInfo;
use rustc_hex::ToHex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use toml::{map::Map, Value};

fn create_account() -> (Secret, Public) {
	let acc = Random
		.generate()
		.expect("secp context has generation capabilities; qed");
	(acc.secret().clone(), acc.public().clone())
}

struct Enode {
	secret: Secret,
	public: Public,
	idx: usize,
}

impl ToString for Enode {
	fn to_string(&self) -> String {
		// Example:
		// enode://30ccdeb8c31972f570e4eea0673cd08cbe7cefc5de1d70119b39c63b1cba33b48e494e9916c0d1eab7d296774f3573da46025d1accdef2f3690bc9e6659a34b4@192.168.0.101:30300
		let port = 30300usize + self.idx;
		format!("enode://{:x}@127.0.0.1:{}", self.public, port)
	}
}

fn generate_enodes(num_nodes: usize) -> BTreeMap<Public, Enode> {
	let mut map = BTreeMap::new();
	for i in 0..num_nodes {
		// Note: node 0 is a regular full node (not a validator) in the testnet setup, so we start at index 1.
		let idx = i + 1;
		let (secret, public) = create_account();
		map.insert(
			public,
			Enode {
				secret,
				public,
				idx,
			},
		);
	}
	map
}

fn to_toml_array(vec: Vec<&str>) -> Value {
	Value::Array(vec.iter().map(|s| Value::String(s.to_string())).collect())
}

fn to_toml<N>(
	net_info: &NetworkInfo<N>,
	enodes_map: &BTreeMap<N, Enode>,
	i: usize,
	config_type: &ConfigType,
	external_ip: Option<&str>,
) -> Value
where
	N: hbbft::NodeIdT + Serialize,
{
	let base_port = 30300i64;
	let base_rpc_port = 8540i64;
	let base_ws_port = 9540i64;

	let mut parity = Map::new();
	match config_type {
		ConfigType::PosdaoSetup => {
			parity.insert("chain".into(), Value::String("./spec/spec.json".into()));
			parity.insert("chain".into(), Value::String("./spec/spec.json".into()));
			let node_data_path = format!("parity-data/node{}", i);
			parity.insert("base_path".into(), Value::String(node_data_path));
		}
		_ => {
			parity.insert("chain".into(), Value::String("spec.json".into()));
			parity.insert("chain".into(), Value::String("spec.json".into()));
			let node_data_path = "data".to_string();
			parity.insert("base_path".into(), Value::String(node_data_path));
		}
	}

	let mut ui = Map::new();
	ui.insert("disable".into(), Value::Boolean(true));

	let mut network = Map::new();
	network.insert("port".into(), Value::Integer(base_port + i as i64));
	network.insert("nat".into(), Value::String("none".into()));
	network.insert("interface".into(), Value::String("local".into()));
	match config_type {
		ConfigType::PosdaoSetup => {
			network.insert(
				"reserved_peers".into(),
				Value::String("parity-data/reserved-peers".into()),
			);
		}
		_ => {
			network.insert(
				"reserved_peers".into(),
				Value::String("reserved-peers".into()),
			);
		}
	}

	if let Some(extip) = external_ip {
		network.insert("allow_ips".into(), Value::String("public".into()));
		network.insert("nat".into(), Value::String(format!("extip:{}", extip)));
	}

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
	if let ConfigType::PosdaoSetup = config_type {
		account.insert(
			"unlock".into(),
			to_toml_array(vec![
				"0xbbcaa8d48289bb1ffcf9808d9aa4b1d215054c78",
				"0x32e4e4c7c5d1cea5db5f9202a9e4d99e56c91a24",
			]),
		);
		account.insert("password".into(), to_toml_array(vec!["config/password"]));
	};

	let mut mining = Map::new();

	match config_type {
		ConfigType::Rpc => {}
		_ => {
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

			// Write the Public Keys
			let pk_serialized = serde_json::to_string(net_info.public_key_map()).unwrap();
			mining.insert("hbbft_public_keys".into(), Value::String(pk_serialized));

			// Write the validator IP Addresses
			let enode_map: BTreeMap<_, _> = enodes_map
				.iter()
				.map(|(n, enode)| (n, enode.to_string()))
				.collect();
			let ips_serialized = serde_json::to_string(&enode_map).unwrap();
			mining.insert(
				"hbbft_validator_ip_addresses".into(),
				Value::String(ips_serialized),
			);
		}
	}

	// Write the Public Key Set
	let pks_serialized = serde_json::to_string(net_info.public_key_set()).unwrap();
	mining.insert("hbbft_public_key_set".into(), Value::String(pks_serialized));

	mining.insert("force_sealing".into(), Value::Boolean(true));
	mining.insert("min_gas_price".into(), Value::Integer(1000000000));
	mining.insert("reseal_on_txs".into(), Value::String("none".into()));
	mining.insert("extra_data".into(), Value::String("Parity".into()));
	mining.insert("reseal_min_period".into(), Value::Integer(0));

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

arg_enum! {
	#[derive(Debug)]
	enum ConfigType {
		PosdaoSetup,
		Docker,
		Rpc
	}
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
		.arg(
			Arg::from_usage("<configtype> 'The ConfigType to use'")
				.possible_values(&ConfigType::variants())
				.index(2),
		)
		.arg(
			Arg::from_usage("<extip> 'Optional external ip to configure'")
				.required(false)
				.index(3),
		)
		.get_matches();

	let num_nodes: usize = matches
		.value_of("INPUT")
		.expect("Number of nodes input required")
		.parse()
		.expect("Input must be of integer type");

	println!("Number of config files to generate: {}", num_nodes);

	let config_type =
		value_t!(matches.value_of("configtype"), ConfigType).unwrap_or(ConfigType::PosdaoSetup);

	let external_ip = matches.value_of("extip");

	let enodes_map = generate_enodes(num_nodes);
	let mut rng = rand::thread_rng();
	let net_infos =
		NetworkInfo::generate_map(enodes_map.keys().cloned().collect::<Vec<_>>(), &mut rng)
			.expect("NetworkInfo generation expected to succeed");

	let mut reserved_peers: String = String::new();
	for (n, info) in net_infos.iter() {
		let enode = enodes_map.get(n).expect("validator id must be mapped");
		writeln!(&mut reserved_peers, "{}", enode.to_string())
			.expect("enode should be written to the reserved peers string");
		//		reserved_peers.push(enode.to_string());
		//		reserved_peers.push('\n');
		let i = enode.idx;
		let file_name = format!("hbbft_validator_{}.toml", i);
		let toml_string =
			toml::to_string(&to_toml(info, &enodes_map, i, &config_type, external_ip))
				.expect("TOML string generation should succeed");
		fs::write(file_name, toml_string).expect("Unable to write config file");

		let file_name = format!("hbbft_validator_key_{}", i);
		fs::write(file_name, enode.secret.to_hex()).expect("Unable to write config file");
	}
	// Write rpc node config
	let rpc_string = toml::to_string(&to_toml(
		net_infos
			.iter()
			.nth(0)
			.expect("At least one NetworkInfo entry must exist")
			.1,
		&enodes_map,
		0,
		&ConfigType::Rpc,
		external_ip,
	))
	.expect("TOML string generation should succeed");
	fs::write("rpc_node.toml", rpc_string).expect("Unable to write rpc config file");

	// Write reserved peers file
	fs::write("reserved-peers", reserved_peers).expect("Unable to write reserved_peers file");
}

#[cfg(test)]
mod tests {
	use super::*;
	use hbbft::crypto::{PublicKeySet, SecretKeyShare};
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
		// Parse and compare the Secret Key Share
		let secret_key_share: SerdeSecret<SecretKeyShare> =
			serde_json::from_str(&options.mining.hbbft_secret_share).unwrap();
		assert_eq!(*net_info.secret_key_share().unwrap(), *secret_key_share);

		// Parse and compare the Public Key Set
		let pks: PublicKeySet = serde_json::from_str(&options.mining.hbbft_public_key_set).unwrap();
		assert_eq!(*net_info.public_key_set(), pks);

		// Parse and compare the Node IDs.
		let ips: BTreeMap<N, String> =
			serde_json::from_str(&options.mining.hbbft_validator_ip_addresses).unwrap();
		assert!(net_info.all_ids().eq(ips.keys()));
	}

	#[test]
	fn test_network_info_serde() {
		let mut rng = rand::thread_rng();
		let enodes_map = generate_enodes(1);
		let net_infos = NetworkInfo::generate_map(enodes_map.keys().cloned(), &mut rng).unwrap();
		let net_info = net_infos.iter().nth(0).unwrap().1;
		let toml_string = toml::to_string(&to_toml(
			net_info,
			&enodes_map,
			1,
			&ConfigType::PosdaoSetup,
			None,
		))
		.unwrap();
		let config: TomlHbbftOptions = toml::from_str(&toml_string).unwrap();
		compare(net_info, &config);
	}
}
