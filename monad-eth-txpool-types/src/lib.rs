// Copyright (C) 2025 Category Labs, Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, TxHash, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use dashmap::{DashMap, Entry};
use monad_eth_block_policy::validation::StaticValidationError;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

pub const DEFAULT_TX_PRIORITY: U256 = U256::from_limbs([0xFFFFu64, 0, 0, 0]);

#[test]
fn test_default_tx_priority() {
    assert_eq!(DEFAULT_TX_PRIORITY, U256::from(0xFFFFu64));
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EthTxPoolEvent {
    pub tx_hash: TxHash,
    pub action: EthTxPoolEventType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EthTxPoolEventType {
    /// The tx was inserted into the txpool.
    Insert {
        address: Address,
        owned: bool,

        #[serde(with = "eth_tx_pool_event_type_tx_serde")]
        tx: TxEnvelope,
    },

    /// The tx was committed and is thus finalized.
    Commit,

    /// The tx was dropped for the attached reason.
    Drop { reason: EthTxPoolDropReason },

    /// The tx timed out and was evicted.
    Evict { reason: EthTxPoolEvictReason },
}

mod eth_tx_pool_event_type_tx_serde {
    use alloy_consensus::TxEnvelope;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S>(tx: &TxEnvelope, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = alloy_rlp::encode(tx);

        <Vec<u8> as Serialize>::serialize(&bytes, serializer)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<TxEnvelope, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <Vec<u8> as Deserialize>::deserialize(deserializer)?;

        alloy_rlp::decode_exact(bytes).map_err(<D::Error as serde::de::Error>::custom)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EthTxPoolDropReason {
    NotWellFormed(StaticValidationError),
    InvalidSignature,
    NonceTooLow,
    FeeTooLow,
    InsufficientBalance,
    ExistingHigherPriority,
    ReplacedByHigherPriority { replacement: TxHash },
    PoolFull,
    PoolNotReady,
    Internal(EthTxPoolInternalDropReason),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EthTxPoolInternalDropReason {
    StateBackendError,
    NotReady,
}

impl EthTxPoolDropReason {
    pub fn as_user_string(&self) -> String {
        match self {
            EthTxPoolDropReason::NotWellFormed(err) => match err {
                StaticValidationError::InvalidChainId { .. } => "Invalid chain ID",
                StaticValidationError::MaxPriorityOverMaxFee { .. } => "Max priority fee too high",
                StaticValidationError::InitCodeLimitExceeded { .. } => {
                    "Init code size limit exceeded"
                }
                StaticValidationError::EncodedLengthLimitExceeded => {
                    "Encoded length limit exceeded"
                }
                StaticValidationError::GasLimitUnderFloorDataGas { .. } => "Gas limit too low",
                StaticValidationError::GasLimitUnderIntrinsicGas { .. } => "Gas limit too low",
                StaticValidationError::GasLimitOverTFMGasLimit { .. } => {
                    "Exceeds transaction gas limit"
                }
                StaticValidationError::GasLimitOverProposalGasLimit { .. } => {
                    "Exceeds transaction gas limit"
                }
                StaticValidationError::InvalidSignature => "Invalid transaction signature",
                StaticValidationError::UnsupportedTransactionType => "Unsupported transaction type",
                StaticValidationError::AuthorizationListEmpty => "EIP7702 authorization list empty",
                StaticValidationError::AuthorizationListLengthLimitExceeded => {
                    "EIP7702 authorization list length limit exceeded"
                }
            },
            EthTxPoolDropReason::InvalidSignature => "Transaction signature is invalid",
            EthTxPoolDropReason::NonceTooLow => "Transaction nonce too low",
            EthTxPoolDropReason::FeeTooLow => "Transaction fee too low",
            EthTxPoolDropReason::InsufficientBalance => "Signer had insufficient balance",
            EthTxPoolDropReason::PoolFull => "Transaction pool is full",
            EthTxPoolDropReason::ExistingHigherPriority => {
                "An existing transaction had higher priority"
            }
            EthTxPoolDropReason::ReplacedByHigherPriority { .. } => {
                "A newer transaction had higher priority"
            }
            EthTxPoolDropReason::PoolNotReady => "Transaction pool is not ready",
            EthTxPoolDropReason::Internal(_) => "Internal error",
        }
        .to_owned()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EthTxPoolEvictReason {
    Expired,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EthTxPoolSnapshot {
    pub txs: HashSet<TxHash>,
}

#[derive(RlpEncodable, RlpDecodable)]
pub struct EthTxPoolIpcTx {
    pub tx: TxEnvelope,
    pub priority: U256,

    // TODO(andr-dev): Pass extra_data to custom sequencers
    pub extra_data: Vec<u8>,
}

impl EthTxPoolIpcTx {
    pub fn new_with_default_priority(tx: TxEnvelope, extra_data: Vec<u8>) -> Self {
        Self {
            tx,
            priority: DEFAULT_TX_PRIORITY,
            extra_data,
        }
    }
}

pub trait EthTxPoolTxInputStream {
    fn poll_txs(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        generate_snapshot: impl Fn() -> EthTxPoolSnapshot,
    ) -> Poll<Vec<EthTxPoolIpcTx>>;

    fn broadcast_tx_events(self: Pin<&mut Self>, events: BTreeMap<TxHash, EthTxPoolEventType>);
}

// --- Bridge state types (moved from monad-rpc) ---

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TxStatus {
    Unknown,
    Tracked,
    Dropped { reason: EthTxPoolDropReason },
    Evicted { reason: EthTxPoolEvictReason },
    Committed,
}

pub type EthTxPoolBridgeEvictionQueue = VecDeque<(Instant, TxHash)>;
pub type TxStatusReceiverSender =
    tokio::sync::oneshot::Sender<tokio::sync::watch::Receiver<TxStatus>>;

const TX_EVICT_DURATION_SECONDS: u64 = 15 * 60;

#[derive(Clone)]
pub struct EthTxPoolBridgeStateView {
    status: Arc<DashMap<TxHash, tokio::sync::watch::Sender<TxStatus>>>,
    address_hashes: Arc<DashMap<Address, HashSet<TxHash>>>,
}

impl EthTxPoolBridgeStateView {
    pub fn get_status_by_hash(&self, hash: &TxHash) -> Option<TxStatus> {
        Some(self.status.get(hash)?.value().borrow().to_owned())
    }

    pub fn get_status_by_address(
        &self,
        address: &Address,
    ) -> Option<HashMap<TxHash, TxStatus>> {
        let hashes = self.address_hashes.get(address)?.value().to_owned();

        let statuses = hashes
            .into_iter()
            .flat_map(|hash| {
                let status = self.status.get(&hash)?.value().borrow().to_owned();
                Some((hash, status))
            })
            .collect();

        Some(statuses)
    }

    pub fn for_testing() -> Self {
        Self {
            status: Default::default(),
            address_hashes: Default::default(),
        }
    }
}

#[derive(Clone)]
pub struct EthTxPoolBridgeState {
    status: Arc<DashMap<TxHash, tokio::sync::watch::Sender<TxStatus>>>,
    hash_address: Arc<DashMap<TxHash, Address>>,
    address_hashes: Arc<DashMap<Address, HashSet<TxHash>>>,
}

impl EthTxPoolBridgeState {
    pub fn new(
        eviction_queue: &mut EthTxPoolBridgeEvictionQueue,
        snapshot: EthTxPoolSnapshot,
    ) -> Self {
        let this = Self {
            status: Default::default(),
            hash_address: Default::default(),
            address_hashes: Default::default(),
        };

        this.apply_snapshot(eviction_queue, snapshot);

        this
    }

    pub fn new_empty() -> Self {
        Self {
            status: Default::default(),
            hash_address: Default::default(),
            address_hashes: Default::default(),
        }
    }

    pub fn create_view(&self) -> EthTxPoolBridgeStateView {
        EthTxPoolBridgeStateView {
            status: Arc::clone(&self.status),
            address_hashes: Arc::clone(&self.address_hashes),
        }
    }

    pub fn add_tx(
        &self,
        eviction_queue: &mut EthTxPoolBridgeEvictionQueue,
        tx: &TxEnvelope,
        tx_status_recv_send: TxStatusReceiverSender,
    ) -> bool {
        let hash = *tx.tx_hash();

        match self.status.entry(hash) {
            Entry::Occupied(mut o) => {
                let mut receiver = o.get_mut().subscribe();

                let known = match &*receiver.borrow() {
                    TxStatus::Unknown => false,
                    TxStatus::Tracked
                    | TxStatus::Dropped { .. }
                    | TxStatus::Evicted { .. }
                    | TxStatus::Committed => true,
                };

                if known {
                    receiver.mark_changed();
                }

                let _ = tx_status_recv_send.send(receiver);

                !known
            }
            Entry::Vacant(v) => {
                let (sender, receiver) = tokio::sync::watch::channel(TxStatus::Unknown);

                v.insert(sender);
                eviction_queue.push_back((Instant::now(), hash));

                let _ = tx_status_recv_send.send(receiver);

                true
            }
        }
    }

    pub fn apply_snapshot(
        &self,
        eviction_queue: &mut EthTxPoolBridgeEvictionQueue,
        snapshot: EthTxPoolSnapshot,
    ) {
        let EthTxPoolSnapshot { mut txs } = snapshot;

        let now = Instant::now();

        eviction_queue.clear();

        self.status.retain(|tx_hash, status| {
            if txs.remove(tx_hash) {
                status.send_if_modified(|tx_status| {
                    *tx_status = TxStatus::Tracked;
                    false
                });
                eviction_queue.push_back((now, *tx_hash));
                return true;
            }

            let Some((tx_hash, address)) = self.hash_address.remove(tx_hash) else {
                return false;
            };

            self.address_hashes.entry(address).and_modify(|hashes| {
                hashes.remove(&tx_hash);
            });

            false
        });

        for tx_hash in txs {
            self.status
                .insert(tx_hash, tokio::sync::watch::channel(TxStatus::Tracked).0);
            eviction_queue.push_back((now, tx_hash));
        }
    }

    pub fn handle_events(
        &self,
        eviction_queue: &mut EthTxPoolBridgeEvictionQueue,
        events: Vec<EthTxPoolEvent>,
    ) {
        let now = Instant::now();

        let mut insert = |tx_hash, tx_status: TxStatus| {
            match self.status.entry(tx_hash) {
                Entry::Occupied(mut o) => {
                    o.get_mut().send_replace(tx_status);
                }
                Entry::Vacant(v) => {
                    v.insert(tokio::sync::watch::channel(tx_status).0);
                    eviction_queue.push_back((now, tx_hash));
                }
            };
        };

        for EthTxPoolEvent { tx_hash, action } in events {
            match action {
                EthTxPoolEventType::Insert {
                    address,
                    owned: _,
                    tx: _,
                } => {
                    insert(tx_hash, TxStatus::Tracked);

                    self.hash_address.entry(tx_hash).insert(address);
                    self.address_hashes
                        .entry(address)
                        .or_default()
                        .insert(tx_hash);
                }
                EthTxPoolEventType::Commit => {
                    insert(tx_hash, TxStatus::Committed);
                }
                EthTxPoolEventType::Drop { reason } => {
                    insert(tx_hash, TxStatus::Dropped { reason });
                }
                EthTxPoolEventType::Evict { reason } => {
                    insert(tx_hash, TxStatus::Evicted { reason });
                }
            }
        }
    }

    pub fn cleanup(&self, eviction_queue: &mut EthTxPoolBridgeEvictionQueue, now: Instant) {
        while eviction_queue
            .front()
            .map(|entry| {
                now.duration_since(entry.0) >= Duration::from_secs(TX_EVICT_DURATION_SECONDS)
            })
            .unwrap_or_default()
        {
            let (_, hash) = eviction_queue.pop_front().unwrap();

            if self.status.remove(&hash).is_none() {
                continue;
            }

            if let Some((hash, address)) = self.hash_address.remove(&hash) {
                if let Some(mut address_hashes) = self.address_hashes.get_mut(&address) {
                    address_hashes.remove(&hash);
                }
            }
        }

        self.address_hashes.retain(|_, hashes| !hashes.is_empty());
    }
}
