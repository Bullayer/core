// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

pub const STATESYNC_VERSION: u32 = 1;

pub fn is_client_compatible(version: u32) -> bool {
    version >= 1 && version <= STATESYNC_VERSION
}
