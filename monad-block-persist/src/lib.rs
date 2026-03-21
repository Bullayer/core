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
    io::{self, ErrorKind},
    marker::PhantomData,
    path::PathBuf,
};

use monad_consensus_types::{
    block::ConsensusBlockHeader,
    payload::{ConsensusBlockBody, ConsensusBlockBodyId},
};
use monad_crypto::certificate_signature::{
    CertificateSignaturePubKey, CertificateSignatureRecoverable,
};
use monad_crypto::hasher::Hash;
use monad_types::{BlockId, ExecutionProtocol};
use monad_validator::signature_collection::SignatureCollection;
use rocksdb::{Options, DB};

/// Subdirectory under the ledger root where RocksDB files live.
pub const BLOCK_LEDGER_ROCKSDB_DIR: &str = "rocksdb";

/// Touching this file under the ledger root signals head pointer updates (for file watchers).
pub const LEDGER_HEAD_NOTIFY_FILE: &str = ".ledger_head_notify";

/// Legacy: headers directory used by the old file-based ledger (no longer written).
pub const BLOCKDB_HEADERS_PATH: &str = "headers";

const KEY_META_PROPOSED: &[u8] = b"v1/meta/proposed_head";
const KEY_META_VOTED: &[u8] = b"v1/meta/voted_head";
const KEY_META_FINALIZED: &[u8] = b"v1/meta/finalized_head";

pub trait BlockPersist<ST, SCT, EPT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
    EPT: ExecutionProtocol,
{
    fn write_bft_header(&mut self, block: &ConsensusBlockHeader<ST, SCT, EPT>) -> io::Result<()>;
    fn write_bft_body(&mut self, payload: &ConsensusBlockBody<EPT>) -> io::Result<()>;

    fn update_proposed_head(&mut self, block_id: &BlockId) -> io::Result<()>;
    fn update_voted_head(&mut self, block_id: &BlockId) -> io::Result<()>;
    fn update_finalized_head(&mut self, block_id: &BlockId) -> io::Result<()>;

    fn read_proposed_head_bft_header(&self) -> io::Result<ConsensusBlockHeader<ST, SCT, EPT>>;
    fn read_bft_header(&self, block_id: &BlockId)
        -> io::Result<ConsensusBlockHeader<ST, SCT, EPT>>;
    fn read_bft_body(
        &self,
        payload_id: &ConsensusBlockBodyId,
    ) -> io::Result<ConsensusBlockBody<EPT>>;
}

fn map_db_err(e: rocksdb::Error) -> io::Error {
    io::Error::other(e.to_string())
}

fn header_key(block_id: &BlockId) -> Vec<u8> {
    let mut k = Vec::with_capacity(5 + 32);
    k.extend_from_slice(b"v1/h/");
    k.extend_from_slice(&block_id.0 .0);
    k
}

fn body_key(body_id: &ConsensusBlockBodyId) -> Vec<u8> {
    let mut k = Vec::with_capacity(5 + 32);
    k.extend_from_slice(b"v1/b/");
    k.extend_from_slice(&body_id.0 .0);
    k
}

fn decode_exact<T: alloy_rlp::Decodable>(
    buf: &[u8],
    context: impl FnOnce() -> String,
) -> io::Result<T> {
    alloy_rlp::decode_exact(buf).map_err(|err| {
        io::Error::other(format!("failed to rlp decode {}, err={:?}", context(), err))
    })
}

fn bump_head_notify(ledger_root: &std::path::Path) {
    let path = ledger_root.join(LEDGER_HEAD_NOTIFY_FILE);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "{nanos}")
        });
}

/// BFT block ledger backed by RocksDB under `{ledger_path}/rocksdb/`.
pub struct RocksDbBlockPersist<ST, SCT, EPT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
    EPT: ExecutionProtocol,
{
    db: DB,
    ledger_root: PathBuf,
    _pd: PhantomData<(ST, SCT, EPT)>,
}

impl<ST, SCT, EPT> RocksDbBlockPersist<ST, SCT, EPT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
    EPT: ExecutionProtocol,
{
    pub fn new(ledger_path: PathBuf) -> Self {
        match std::fs::create_dir_all(&ledger_path) {
            Ok(_) => (),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => (),
            Err(e) => panic!("{}", e),
        }

        let db_path = ledger_path.join(BLOCK_LEDGER_ROCKSDB_DIR);
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DB::open(&opts, db_path).expect("open rocksdb block ledger");

        Self {
            db,
            ledger_root: ledger_path,
            _pd: PhantomData,
        }
    }
}

impl<ST, SCT, EPT> BlockPersist<ST, SCT, EPT> for RocksDbBlockPersist<ST, SCT, EPT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
    EPT: ExecutionProtocol,
{
    fn write_bft_header(&mut self, block: &ConsensusBlockHeader<ST, SCT, EPT>) -> io::Result<()> {
        let key = header_key(&block.get_id());
        if self.db.get(&key).map_err(map_db_err)?.is_some() {
            return Ok(());
        }
        let encoded = alloy_rlp::encode(block);
        self.db.put(&key, encoded).map_err(map_db_err)
    }

    fn write_bft_body(&mut self, body: &ConsensusBlockBody<EPT>) -> io::Result<()> {
        let key = body_key(&body.get_id());
        if self.db.get(&key).map_err(map_db_err)?.is_some() {
            return Ok(());
        }
        let encoded = alloy_rlp::encode(body);
        self.db.put(&key, encoded).map_err(map_db_err)
    }

    fn update_proposed_head(&mut self, block_id: &BlockId) -> io::Result<()> {
        self.db
            .put(KEY_META_PROPOSED, block_id.0 .0)
            .map_err(map_db_err)?;
        bump_head_notify(&self.ledger_root);
        Ok(())
    }

    fn update_voted_head(&mut self, block_id: &BlockId) -> io::Result<()> {
        self.db
            .put(KEY_META_VOTED, block_id.0 .0)
            .map_err(map_db_err)?;
        bump_head_notify(&self.ledger_root);
        Ok(())
    }

    fn update_finalized_head(&mut self, block_id: &BlockId) -> io::Result<()> {
        self.db
            .put(KEY_META_FINALIZED, block_id.0 .0)
            .map_err(map_db_err)?;
        bump_head_notify(&self.ledger_root);
        Ok(())
    }

    fn read_proposed_head_bft_header(&self) -> io::Result<ConsensusBlockHeader<ST, SCT, EPT>> {
        let Some(raw) = self.db.get(KEY_META_PROPOSED).map_err(map_db_err)? else {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "ledger has no proposed head",
            ));
        };
        if raw.len() != 32 {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "corrupt proposed head id",
            ));
        }
        let mut h = [0u8; 32];
        h.copy_from_slice(&raw);
        self.read_bft_header(&BlockId(Hash(h)))
    }

    fn read_bft_header(
        &self,
        block_id: &BlockId,
    ) -> io::Result<ConsensusBlockHeader<ST, SCT, EPT>> {
        let key = header_key(block_id);
        let Some(buf) = self.db.get(&key).map_err(map_db_err)? else {
            return Err(io::Error::new(ErrorKind::NotFound, "bft header not found"));
        };
        decode_exact(&buf, || format!("ledger bft header, block_id={block_id:?}"))
    }

    fn read_bft_body(&self, body_id: &ConsensusBlockBodyId) -> io::Result<ConsensusBlockBody<EPT>> {
        let key = body_key(body_id);
        let Some(buf) = self.db.get(&key).map_err(map_db_err)? else {
            return Err(io::Error::new(ErrorKind::NotFound, "bft body not found"));
        };
        decode_exact(&buf, || format!("ledger bft body, body_id={body_id:?}"))
    }
}
