use crate::NodeId;
use hbbft::crypto::Signature;
use hbbft::threshold_sign::ThresholdSign;
use hbbft::NetworkInfo;
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
