// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

// ExecutionStateBackend bridges the in-process ExecutionDb to the existing StateBackend trait.
// This will be implemented when modifying monad-node/src/main.rs (task: modify-ledger-and-node).
//
// The adapter reads from ExecutionDb to fulfil StateBackend::get_account_statuses,
// get_execution_result, etc. For now this is a placeholder module.
