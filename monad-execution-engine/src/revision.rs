// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

/// Corresponds to C++ monad_revision / SWITCH_MONAD_TRAITS dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MonadRevision {
    Zero,
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvmRevision {
    Cancun,
    Prague,
    Osaka,
}

impl MonadRevision {
    pub fn evm_revision(&self) -> EvmRevision {
        match self {
            Self::Nine | Self::Next => EvmRevision::Osaka,
            Self::Four | Self::Five | Self::Six | Self::Seven | Self::Eight => EvmRevision::Prague,
            _ => EvmRevision::Cancun,
        }
    }

    pub fn from_timestamp(timestamp: u64, chain_revision_timestamps: &[u64]) -> Self {
        let revisions = [
            Self::Zero,
            Self::One,
            Self::Two,
            Self::Three,
            Self::Four,
            Self::Five,
            Self::Six,
            Self::Seven,
            Self::Eight,
            Self::Nine,
            Self::Next,
        ];

        let mut result = Self::Zero;
        for (i, &ts) in chain_revision_timestamps.iter().enumerate() {
            if i < revisions.len() && timestamp >= ts {
                result = revisions[i];
            }
        }
        result
    }
}
