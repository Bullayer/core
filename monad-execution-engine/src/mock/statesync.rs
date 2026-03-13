// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use crate::statesync::deletion_tracker::FinalizedDeletions;
use crate::statesync::server::handle_sync_request;
use crate::statesync::{
    StateSyncError, StateSyncProvider, StateSyncRequest, StateSyncResult, StateSyncTraversable,
};

/// Mock StateSyncProvider: composes a StateSyncTraversable + FinalizedDeletions
/// for handling peer sync requests in Live mode.
pub struct MockStateSyncProvider {
    db: Box<dyn StateSyncTraversable>,
    deletions: FinalizedDeletions,
}

impl MockStateSyncProvider {
    pub fn new(db: Box<dyn StateSyncTraversable>) -> Self {
        Self {
            db,
            deletions: FinalizedDeletions::new(),
        }
    }

    pub fn deletions_mut(&mut self) -> &mut FinalizedDeletions {
        &mut self.deletions
    }
}

impl StateSyncProvider for MockStateSyncProvider {
    fn handle_sync_request(
        &self,
        request: &StateSyncRequest,
    ) -> Result<StateSyncResult, StateSyncError> {
        handle_sync_request(request, &*self.db, &self.deletions)
    }
}
