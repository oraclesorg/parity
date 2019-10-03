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

//! A signer used by Engines which need to sign messages.

use ethereum_types::{H256, Address};
use ethkey::{self, Signature};
use ethkey::crypto::ecies;

/// Everything that an Engine needs to sign messages.
pub trait EngineSigner: Send + Sync {
	/// Sign a consensus message hash.
	fn sign(&self, hash: H256) -> Result<Signature, ethkey::Error>;

	/// Signing address
	fn address(&self) -> Address;

	/// Decrypt a message that was encrypted to this signer's key.
	fn decrypt(&self, auth_data: &[u8], cipher: &[u8]) -> Result<Vec<u8>, ethkey::crypto::Error>;

	/// The signer's public key, if available.
	fn public(&self) -> Option<ethkey::Public>;
}

/// Creates a new `EngineSigner` from given key pair.
pub fn from_keypair(keypair: ethkey::KeyPair) -> Box<dyn EngineSigner> {
	Box::new(Signer(keypair))
}

struct Signer(ethkey::KeyPair);

impl EngineSigner for Signer {
	fn sign(&self, hash: H256) -> Result<Signature, ethkey::Error> {
		ethkey::sign(self.0.secret(), &hash)
	}

	fn decrypt(&self, auth_data: &[u8], cipher: &[u8]) -> Result<Vec<u8>, ethkey::crypto::Error> {
		ecies::decrypt(self.0.secret(), auth_data, cipher)
	}

	fn address(&self) -> Address {
		self.0.address()
	}

	fn public(&self) -> Option<ethkey::Public> {
		Some(*self.0.public())
	}
}
