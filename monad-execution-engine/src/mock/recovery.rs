// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::Address;

use crate::traits::SignatureRecovery;
use crate::types::Transaction;

/// Mock signature recovery that generates deterministic addresses from transaction index.
pub struct MockSignatureRecovery;

impl MockSignatureRecovery {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockSignatureRecovery {
    fn default() -> Self {
        Self::new()
    }
}

impl SignatureRecovery for MockSignatureRecovery {
    fn recover_senders(&self, txs: &[Transaction]) -> Vec<Address> {
        txs.iter()
            .enumerate()
            .map(|(i, _tx)| {
                let mut addr = [0u8; 20];
                // Use i+1 so that index 0 doesn't produce Address::ZERO
                let sender_id = (i + 1) as u16;
                addr[0] = (sender_id >> 8) as u8;
                addr[1] = sender_id as u8;
                Address::from(addr)
            })
            .collect()
    }

    fn recover_authorities(&self, txs: &[Transaction]) -> Vec<Vec<Option<Address>>> {
        txs.iter().map(|_| Vec::new()).collect()
    }
}
