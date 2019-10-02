use crate::NodeId;
use hbbft::crypto::Signature;
use hbbft::threshold_sign::ThresholdSign;
use hbbft::NetworkInfo;
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use std::result;
use std::sync::Arc;

pub use hbbft::threshold_sign::{Message, Result};

pub type Step = hbbft::threshold_sign::Step<NodeId>;

/// The status of sealing an individual block.
pub enum Sealing {
	/// Threshold signature shares are still being collected.
	Ongoing(ThresholdSign<NodeId>),
	/// The shares have been combined, and the signature is ready to be used as the block's seal.
	Complete(Signature),
}

impl Sealing {
	/// Returns a new `Ongoing` state, ready to start collecting signature shares.
	pub fn new(netinfo: NetworkInfo<NodeId>) -> Self {
		Sealing::Ongoing(ThresholdSign::new(Arc::new(netinfo)))
	}

	/// Handles a message containing a signature share.
	pub fn handle_message(&mut self, sender_id: &NodeId, message: Message) -> Result<Step> {
		match self {
			Sealing::Ongoing(ts) => ts.handle_message(sender_id, message),
			Sealing::Complete(_) => Ok(Step::default()),
		}
	}

	/// Sets the `hash` as the document to be signed, and creates a signature share.
	pub fn sign<M: AsRef<[u8]>>(&mut self, hash: M) -> Result<Step> {
		let ts = match self {
			Sealing::Ongoing(ts) => ts,
			Sealing::Complete(_) => return Ok(Step::default()),
		};
		ts.set_document(hash)?;
		ts.sign()
	}

	/// Returns the combined signature, if it is ready.
	pub fn signature(&self) -> Option<&Signature> {
		match self {
			Sealing::Ongoing(_) => None,
			Sealing::Complete(sig) => Some(sig),
		}
	}
}

/// Wrapper for `Signature` to simplify RLP encoding and decoding.
#[derive(PartialEq, Debug)]
pub struct RlpSig<T>(pub T);

impl<'a> Encodable for RlpSig<&'a Signature> {
	fn rlp_append(&self, s: &mut RlpStream) {
		s.encoder().encode_value(&self.0.to_bytes());
	}
}

const RLP_ERR: &str = "RLP bytes don't encode a valid signature";

impl Decodable for RlpSig<Signature> {
	fn decode(rlp: &Rlp) -> result::Result<Self, DecoderError> {
		let mut seal_bytes = [0u8; 96];
		seal_bytes.copy_from_slice(rlp.data()?);
		let sig = Signature::from_bytes(seal_bytes).map_err(|_| DecoderError::Custom(RLP_ERR))?;
		Ok(RlpSig(sig))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rand;
	use rlp;

	#[test]
	fn test_rlp_signature() {
		let sig: Signature = rand::random();
		let encoded = rlp::encode(&RlpSig(&sig));
		let decoded: RlpSig<Signature> = rlp::decode(&encoded).expect("decode RlpSignature");
		assert_eq!(decoded.0, sig);
	}
}
