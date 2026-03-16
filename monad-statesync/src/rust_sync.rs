// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Pure Rust replacement for ffi::StateSync.
// Uses StateSyncClientContext directly instead of C++ FFI.

use std::{
    collections::VecDeque,
    ops::DerefMut,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use alloy_consensus::Header;
use futures::{Future, Stream};
use monad_crypto::certificate_signature::PubKey;
use monad_execution_engine::statesync::client_context::StateSyncClientContext;
use monad_execution_engine::statesync::protocol::build_sync_request;
use monad_execution_engine::statesync::types::{SyncDone, SyncUpsertType};
use monad_execution_engine::statesync::{StateSyncApplier, StateSyncApplierDb};
use monad_executor_glue::{
    SessionId, StateSyncBadVersion, StateSyncRequest as GlueRequest,
    StateSyncResponse, StateSyncUpsertType as GlueUpsertType, SELF_STATESYNC_VERSION,
};
use monad_types::{NodeId, SeqNum};

use crate::outbound_requests::{OutboundRequests, RequestPollResult};

const NUM_PREFIXES: u64 = 256;
const PREFIX_BYTES: u8 = 1;

pub(crate) enum SyncRequest<PT: PubKey> {
    Request((NodeId<PT>, GlueRequest)),
    DoneSync(Header),
    Completion((NodeId<PT>, SessionId)),
}

#[derive(Clone, Copy, Default)]
struct Progress {
    start_target: Option<SeqNum>,
    end_target: Option<SeqNum>,
    min_until_guess: Option<SeqNum>,
    current_progress: Option<u64>,
}

impl Progress {
    fn update_target(&mut self, target: &Header) {
        self.end_target = Some(SeqNum(target.number));
        if self.min_until_guess.is_none() {
            self.min_until_guess = Some(SeqNum(target.number.max(10_000) - 10_000));
        }
    }

    fn update_handled_request(&mut self, request: &GlueRequest) {
        assert_eq!(self.end_target, Some(SeqNum(request.target)));
        let min_until_guess = self.min_until_guess.expect("self.end_target exists").0;

        if self.start_target.is_none() {
            self.start_target = Some(SeqNum(request.from));
        }
        let start_target = self.start_target.expect("start_target set").0;

        if self.current_progress.is_none() {
            self.current_progress = Some(request.from * NUM_PREFIXES);
        }

        if request.until >= min_until_guess {
            let adjusted_from = if request.from <= min_until_guess {
                start_target
            } else {
                request.from
            };
            *self
                .current_progress
                .as_mut()
                .expect("current_progress was set") += request.until - adjusted_from;
        }
    }

    fn update_reached_target(&mut self, target: &Header) {
        assert_eq!(self.end_target, Some(SeqNum(target.number)));
        self.start_target = Some(SeqNum(target.number));
        self.current_progress = None;
    }

    fn estimate(&self) -> Option<SeqNum> {
        let start_target = self.start_target?;
        let end_target = self.end_target?;
        if start_target == end_target {
            return Some(end_target);
        }
        assert!(end_target > start_target);
        Some(SeqNum(self.current_progress? / NUM_PREFIXES))
    }
}

pub(crate) struct RustStateSync<PT: PubKey> {
    ctx: StateSyncClientContext,
    outbound_requests: OutboundRequests<PT>,
    current_target: Option<Header>,
    next_target: Option<Header>,
    progress: Arc<Mutex<Progress>>,

    pending_events: VecDeque<SyncRequest<PT>>,
    sleep_future: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<PT: PubKey> RustStateSync<PT> {
    pub fn new(
        applier_db: Box<dyn StateSyncApplierDb>,
        init_peers: &[NodeId<PT>],
        max_parallel_requests: usize,
        request_timeout: Duration,
    ) -> Self {
        Self {
            ctx: StateSyncClientContext::new(applier_db),
            outbound_requests: OutboundRequests::new(
                max_parallel_requests,
                request_timeout,
                init_peers,
            ),
            current_target: None,
            next_target: None,
            progress: Arc::new(Mutex::new(Progress::default())),
            pending_events: VecDeque::new(),
            sleep_future: None,
        }
    }

    pub fn update_target(&mut self, target: Header) {
        if let Some(old_target) = &self.current_target {
            assert!(old_target.number < target.number);
        }

        if self.current_target.is_none() {
            tracing::debug!(new_target =? target, "updating statesync target");
            self.ctx.set_target(&target);
            self.progress.lock().unwrap().update_target(&target);
            self.current_target = Some(target.clone());

            let pending = self.ctx.take_pending_requests();
            for prefix in pending {
                let (progress, old_target) = self.ctx.progress[prefix as usize];
                let request = build_sync_request(
                    prefix,
                    PREFIX_BYTES,
                    target.number,
                    progress,
                    old_target,
                );
                let glue_request = to_glue_request(&request);
                self.outbound_requests.queue_request(glue_request);
            }
        } else {
            self.next_target.replace(target);
        }
    }

    pub fn handle_response(&mut self, from: NodeId<PT>, response: StateSyncResponse) {
        if !self.is_trusted_peer(&from) {
            tracing::warn!(?from, ?response, "dropping statesync response from untrusted peer");
            return;
        }

        if !response.version.is_compatible() {
            tracing::debug!(?from, ?response, "dropping statesync response, version incompatible");
            return;
        }

        for response in self.outbound_requests.handle_response(from, response) {
            assert!(self.current_target.is_some());

            for upsert in &response.response {
                let upsert_type = convert_upsert_type(upsert.upsert_type);
                let result =
                    self.ctx
                        .apply_upsert(response.request.prefix, upsert_type, &upsert.data);
                assert!(result, "failed upsert for response: {:?}", &response);
            }

            self.pending_events.push_back(SyncRequest::Completion((
                from,
                SessionId(response.nonce),
            )));

            if response.response_n != 0 {
                self.ctx.handle_done(SyncDone {
                    success: true,
                    prefix: response.request.prefix,
                    n: response.response_n,
                });
                self.progress
                    .lock()
                    .unwrap()
                    .update_handled_request(&response.request);

                let pending = self.ctx.take_pending_requests();
                let target = self.current_target.as_ref().unwrap();
                for prefix in pending {
                    let (progress, old_target) = self.ctx.progress[prefix as usize];
                    let request = build_sync_request(
                        prefix,
                        PREFIX_BYTES,
                        target.number,
                        progress,
                        old_target,
                    );
                    let glue_request = to_glue_request(&request);
                    self.outbound_requests.queue_request(glue_request);
                }
            }

            if self.ctx.has_reached_target() {
                let finalized = self.ctx.finalize();
                assert!(finalized, "state root doesn't match, are peers trusted?");

                let target = self.current_target.take().expect("target should be set");
                self.current_target = self.next_target.take();

                if let Some(ref new_target) = self.current_target {
                    tracing::debug!(
                        "statesync reached target {:?}, next target {:?}",
                        target,
                        new_target
                    );
                    self.ctx.set_target(new_target);
                    self.progress.lock().unwrap().update_target(new_target);

                    let pending = self.ctx.take_pending_requests();
                    for prefix in pending {
                        let (progress, old_target) = self.ctx.progress[prefix as usize];
                        let request = build_sync_request(
                            prefix,
                            PREFIX_BYTES,
                            new_target.number,
                            progress,
                            old_target,
                        );
                        let glue_request = to_glue_request(&request);
                        self.outbound_requests.queue_request(glue_request);
                    }
                } else {
                    tracing::debug!(?target, "done statesync");
                    self.progress.lock().unwrap().update_reached_target(&target);
                    self.pending_events.push_back(SyncRequest::DoneSync(target));
                }
                break;
            }
        }
    }

    pub fn handle_bad_version(&mut self, from: NodeId<PT>, bad_version: StateSyncBadVersion) {
        if !self.is_trusted_peer(&from) {
            tracing::warn!(?from, ?bad_version, "dropping bad version from untrusted peer");
            return;
        }
        self.outbound_requests.handle_bad_version(from, bad_version);
    }

    pub fn handle_not_whitelisted(&mut self, from: NodeId<PT>) {
        if !self.is_trusted_peer(&from) {
            tracing::warn!(?from, "dropping not whitelisted from untrusted peer");
            return;
        }
        self.outbound_requests.handle_not_whitelisted(from);
    }

    pub fn expand_upstream_peers(&mut self, new_peers: &[NodeId<PT>]) {
        self.outbound_requests.expand_upstream_peers(new_peers);
    }

    pub fn progress_estimate(&self) -> Option<SeqNum> {
        self.progress.lock().unwrap().estimate()
    }

    fn is_trusted_peer(&self, peer: &NodeId<PT>) -> bool {
        self.outbound_requests.is_trusted_peer(peer)
    }
}

impl<PT: PubKey> Stream for RustStateSync<PT> {
    type Item = SyncRequest<PT>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.deref_mut();

        if let Some(event) = this.pending_events.pop_front() {
            return Poll::Ready(Some(event));
        }

        match this.outbound_requests.poll() {
            RequestPollResult::Request(peer, request) => {
                this.sleep_future = None;
                Poll::Ready(Some(SyncRequest::Request((peer, request))))
            }
            RequestPollResult::Timer(Some(instant)) => {
                match this.sleep_future.as_mut() {
                    Some(s) if s.deadline() != instant.into() => s.as_mut().reset(instant.into()),
                    Some(_) => {}
                    None => {
                        this.sleep_future =
                            Some(Box::pin(tokio::time::sleep_until(instant.into())));
                    }
                }
                let sleep_future = this.sleep_future.as_mut().unwrap();
                if sleep_future.as_mut().poll(cx).is_ready() {
                    this.sleep_future = None;
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            RequestPollResult::Timer(None) => {
                this.sleep_future = None;
                Poll::Pending
            }
        }
    }
}

fn to_glue_request(
    req: &monad_execution_engine::statesync::types::StateSyncRequest,
) -> GlueRequest {
    GlueRequest {
        version: SELF_STATESYNC_VERSION,
        prefix: req.prefix,
        prefix_bytes: req.prefix_bytes,
        target: req.target,
        from: req.from,
        until: req.until,
        old_target: req.old_target,
    }
}

fn convert_upsert_type(t: GlueUpsertType) -> SyncUpsertType {
    match t {
        GlueUpsertType::Code => SyncUpsertType::Code,
        GlueUpsertType::Account => SyncUpsertType::Account,
        GlueUpsertType::Storage => SyncUpsertType::Storage,
        GlueUpsertType::AccountDelete => SyncUpsertType::AccountDelete,
        GlueUpsertType::StorageDelete => SyncUpsertType::StorageDelete,
        GlueUpsertType::Header => SyncUpsertType::Header,
    }
}
