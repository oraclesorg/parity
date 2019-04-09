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

//! Provides CallContract and RegistryInfo traits

use bytes::Bytes;
use ethereum_types::Address;
use types::header::Header;
use types::ids::BlockId;

/// Provides `call_contract` method
pub trait CallContract {
	/// Like `call`, but with various defaults. Designed to be used for calling contracts.
	fn call_contract(&self, id: BlockId, address: Address, data: Bytes) -> Result<Bytes, String>;

	/// Makes a constant call to a contract, at the block corresponding to the given header. Fails if the block is not
	/// in the database.
	fn call_contract_at(&self, header: &Header, address: Address, data: Bytes) -> Result<Bytes, String>;
}

/// Provides information on a blockchain service and it's registry
pub trait RegistryInfo {
	/// Get the address of a particular blockchain service, if available.
	fn registry_address(&self, name: String, block: BlockId) -> Option<Address>;
}
