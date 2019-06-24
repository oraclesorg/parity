extern crate clap;
extern crate hbbft;
extern crate rand;
extern crate serde;
extern crate toml;
extern crate ethcore;

use clap::{App, Arg};
use hbbft::crypto::{serde_impl::SerdeSecret};
use hbbft::NetworkInfo;
use serde::Serialize;
use std::fs;
use toml::{map::Map, Value};

fn to_toml<N>(net_info: &NetworkInfo<N>) -> Value
where
	N: hbbft::NodeIdT + Serialize,
{
	let mut server = Map::new();

	// Write the Secret Key Share
	let wrapper = SerdeSecret(net_info.secret_key_share().unwrap());
	let sks_serialized = serde_json::to_string(&wrapper).unwrap();
	server.insert("hbbft_secret_share".into(), Value::String(sks_serialized));

	// Write the Secret Key
	let wrapper = SerdeSecret(net_info.secret_key());
	let sk_serialized = serde_json::to_string(&wrapper).unwrap();
	server.insert("hbbft_secret_key".into(), Value::String(sk_serialized));

	// Write the Public Key Set
	let pks_serialized = serde_json::to_string(net_info.public_key_set()).unwrap();
	server.insert("hbbft_public_key_set".into(), Value::String(pks_serialized));

	// Write the Public Keys
	let pk_serialized = serde_json::to_string(net_info.public_key_map()).unwrap();
	server.insert("hbbft_public_keys".into(), Value::String(pk_serialized));

	let mut map = Map::new();
	map.insert("mining".into(), Value::Table(server));
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
		let file_name = format!("hbbft_validator_{}.toml", i);
		let toml_string =
			toml::to_string(&to_toml(info)).expect("TOML string generation should succeed");
		fs::write(file_name, toml_string).expect("Unable to write config file");
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rand;
	use std::collections::BTreeMap;
	use serde::Deserialize;
	use hbbft::crypto::{PublicKey, PublicKeySet, SecretKey, SecretKeyShare};

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

		// For debugging toml output:
		//println!("{}", toml_string);

		let config: TomlHbbftOptions = toml::from_str(&toml_string).unwrap();
		compare(net_info, &config);
	}
}
