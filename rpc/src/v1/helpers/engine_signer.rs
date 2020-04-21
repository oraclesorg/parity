// Copyright 2015-2020 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use std::sync::Arc;

use accounts::AccountProvider;
use ethkey::Password;
use crypto::publickey::{Address, Message, Public, Signature, Error};

use std::time::{Duration, Instant};
use log::trace;

/// An implementation of EngineSigner using internal account management.
pub struct EngineSigner {
	accounts: Arc<AccountProvider>,
	address: Address,
	password: Password,
}

impl EngineSigner {
	/// Creates new `EngineSigner` given account manager and account details.
	pub fn new(accounts: Arc<AccountProvider>, address: Address, password: Password) -> Self {
		EngineSigner { accounts, address, password }
	}
}

impl engine::signer::EngineSigner for EngineSigner {
	fn sign(&self, message: Message) -> Result<Signature, Error> {
		let took_ms = |elapsed: &Duration| {
			elapsed.as_secs() * 1000 + elapsed.subsec_nanos() as u64 / 1_000_000
		};
		let start = Instant::now();
		let result = match self.accounts.sign(self.address, Some(self.password.clone()), message) {
			Ok(ok) => Ok(ok),
			Err(_) => Err(Error::InvalidSecretKey),
		};
		let took = start.elapsed();
		trace!(target: "engine", "EngineSigner::sign(2) took {} ms", took_ms(&took));
		result
	}

	fn decrypt(&self, auth_data: &[u8], cipher: &[u8]) -> Result<Vec<u8>, Error> {
		let took_ms = |elapsed: &Duration| {
			elapsed.as_secs() * 1000 + elapsed.subsec_nanos() as u64 / 1_000_000
		};
		let start = Instant::now();
		let result = self.accounts.decrypt(self.address, None, auth_data, cipher).map_err(|e| {
			warn!("Unable to decrypt message: {:?}", e);
			Error::InvalidMessage
		});
		let took = start.elapsed();
		trace!(target: "engine", "EngineSigner::decrypt (rpc/src/v1/helpers/engine_signer.rs) took {} ms", took_ms(&took));
		result
	}

	fn address(&self) -> Address {
		self.address
	}

	fn public(&self) -> Option<Public> {
		let took_ms = |elapsed: &Duration| {
			elapsed.as_secs() * 1000 + elapsed.subsec_nanos() as u64 / 1_000_000
		};
		let start = Instant::now();
		let result = self.accounts.account_public(self.address, &self.password).ok();
		let took = start.elapsed();
		trace!(target: "engine", "EngineSigner::public (rpc/src/v1/helpers/engine_signer.rs) took {} ms", took_ms(&took));
		result
	}
}

