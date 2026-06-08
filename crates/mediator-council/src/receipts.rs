//! Hash-chained receipt helpers for the council deliberation.

use mediator_types::{Receipt, Verdict};
use sha2::{Digest, Sha256};

pub struct CouncilReceiptChain {
    seq: u64,
    prev_hash: String,
}

impl CouncilReceiptChain {
    pub fn new() -> Self {
        Self {
            seq: 0,
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        }
    }

    pub fn append(
        &mut self,
        op: &str,
        detail: serde_json::Value,
        verdict: Option<Verdict>,
    ) -> Receipt {
        let payload = format!("{}{}{}", self.seq, self.prev_hash, detail);
        let hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let receipt = Receipt {
            seq: self.seq,
            prev_hash: self.prev_hash.clone(),
            hash: hash.clone(),
            op: op.to_string(),
            detail,
            verdict,
        };
        self.seq += 1;
        self.prev_hash = hash;
        receipt
    }

    pub fn chain(&self) -> &str {
        &self.prev_hash
    }
}
