use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_consensus::{ReceiptEnvelope, Typed2718, TxType};
use alloy_primitives::{Address, B256, U256};
use reth_primitives_traits::Account;
use revm::database::states::{
    plain_account::StorageWithOriginalValues,
    reverts::{AccountInfoRevert, AccountRevert, RevertToSlot, Reverts},
    AccountStatus, BundleState, StorageSlot,
};
use revm::database::BundleAccount;
use revm::primitives::HashMap;
use revm::state::AccountInfo;

use monad_eth_types::EthAccount;
use monad_execution_engine::types::{CodeMap, StateDeltas};

/// Convert a monad `EthAccount` to reth's `Account`.
pub fn monad_to_reth_account(acc: &EthAccount) -> Account {
    Account {
        nonce: acc.nonce,
        balance: acc.balance,
        bytecode_hash: acc.code_hash.map(B256::from),
    }
}

/// Convert a reth `Account` to monad's `EthAccount`.
pub fn reth_to_monad_account(acc: &Account) -> EthAccount {
    let code_hash = acc
        .bytecode_hash
        .and_then(|h| if h == KECCAK_EMPTY { None } else { Some(h.0) });
    EthAccount {
        nonce: acc.nonce,
        balance: acc.balance,
        code_hash,
        is_delegated: false,
    }
}

/// Convert a monad `EthAccount` to revm `AccountInfo`.
fn monad_to_account_info(acc: &EthAccount) -> AccountInfo {
    AccountInfo {
        balance: acc.balance,
        nonce: acc.nonce,
        code_hash: acc
            .code_hash
            .map(B256::from)
            .unwrap_or(KECCAK_EMPTY),
        account_id: None,
        code: None,
    }
}

/// Convert an `alloy_consensus::ReceiptEnvelope` to a `reth_ethereum_primitives::Receipt`.
pub fn receipt_envelope_to_reth(envelope: &ReceiptEnvelope) -> reth_ethereum_primitives::Receipt {
    reth_ethereum_primitives::Receipt {
        tx_type: TxType::try_from(envelope.ty()).unwrap_or(TxType::Legacy),
        success: envelope.status(),
        cumulative_gas_used: envelope.cumulative_gas_used(),
        logs: envelope.logs().to_vec(),
    }
}

/// Convert monad `StateDeltas` + `CodeMap` to revm `BundleState`.
///
/// Preserves old/new account values, storage diffs, **and reverts** so reth's
/// `StateWriter` can produce proper changesets and history entries.
pub fn state_deltas_to_bundle(
    state_deltas: &StateDeltas,
    code: &CodeMap,
) -> BundleState {
    let mut state: HashMap<Address, BundleAccount> = Default::default();
    let mut block_reverts: Vec<(Address, AccountRevert)> = Vec::new();
    let mut reverts_size: usize = 0;

    for (address, delta) in state_deltas {
        let original_info = delta.old_account.as_ref().map(monad_to_account_info);
        let present_info = delta.new_account.as_ref().map(monad_to_account_info);

        let status = match (&delta.old_account, &delta.new_account) {
            (None, Some(_)) => AccountStatus::InMemoryChange,
            (Some(_), None) => AccountStatus::Destroyed,
            (Some(_), Some(_)) => AccountStatus::Changed,
            (None, None) => {
                if delta.storage.is_empty() {
                    continue;
                }
                AccountStatus::InMemoryChange
            }
        };

        let mut storage: StorageWithOriginalValues = Default::default();
        let mut storage_revert: HashMap<U256, RevertToSlot> = Default::default();
        for (slot, sd) in &delta.storage {
            let key = U256::from_be_bytes(slot.0);
            storage.insert(
                key,
                StorageSlot::new_changed(
                    U256::from_be_bytes(sd.old_value.0),
                    U256::from_be_bytes(sd.new_value.0),
                ),
            );
            storage_revert.insert(key, RevertToSlot::Some(U256::from_be_bytes(sd.old_value.0)));
        }

        let account_revert = match (&delta.old_account, &delta.new_account) {
            (None, Some(_)) => AccountInfoRevert::DeleteIt,
            (Some(old), None) => AccountInfoRevert::RevertTo(monad_to_account_info(old)),
            (Some(old), Some(_)) => AccountInfoRevert::RevertTo(monad_to_account_info(old)),
            (None, None) => AccountInfoRevert::DoNothing,
        };

        let previous_status = if delta.old_account.is_some() {
            AccountStatus::Loaded
        } else {
            AccountStatus::LoadedNotExisting
        };

        let revert = AccountRevert {
            account: account_revert,
            storage: storage_revert,
            previous_status,
            wipe_storage: delta.incarnation_changed,
        };
        reverts_size += revert.size_hint();
        block_reverts.push((*address, revert));

        let bundle_account = BundleAccount {
            info: present_info,
            original_info,
            storage,
            status,
        };
        state.insert(*address, bundle_account);
    }

    let mut contracts: HashMap<B256, revm::bytecode::Bytecode> = Default::default();
    for (hash, bytecode) in code {
        contracts.insert(
            *hash,
            revm::bytecode::Bytecode::new_raw(bytecode.clone().into()),
        );
    }

    BundleState {
        state,
        contracts,
        reverts: Reverts::new(vec![block_reverts]),
        state_size: 0,
        reverts_size,
    }
}
