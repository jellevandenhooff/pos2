use std::net::SocketAddr;
use std::thread::ThreadId;
use std::{ffi::c_void, fmt::Debug, hash::Hasher, io::Write, sync::Arc};

use axum::{Extension, Json};
use rand::{Rng, rng};
use zerocopy::{FromBytes, IntoBytes};

use serde::{Deserialize, Serialize};

use std::ops::{Index, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo};

use bytes::{Buf, Bytes, BytesMut};
use culprit::{Culprit, Result, ResultExt};
use parking_lot::Mutex;
use sqlite_plugin::flags::{self, LockLevel, OpenOpts};

use tracing::info;

use crate::replvfs::structs::{self, WalIndexHeaderTail};

use super::ErrCtx;

use super::VfsFile;

const PAGESIZE: usize = 4096;

const READMARK_NOT_USED: u32 = 0xffffffff;

#[derive(Debug)]
struct WalIndex {
    first_page: std::cell::UnsafeCell<[u8; 32768]>,
}

impl WalIndex {
    fn new() -> Self {
        Self {
            first_page: std::cell::UnsafeCell::new([0u8; 32768]),
            // let initial_wal_index_data = vec![0; structs::WAL_INDEX_INITIAL_SIZE];
        }
    }

    fn void_pointer_for_sqlite(&self) -> *mut c_void {
        self.first_page.get() as *mut c_void
    }

    /*
    fn get_index(&mut self) -> structs::WalIndexInfo {
        // we assume (tm) that sqlite does not access the
        // wal index from multiple threads. so when we access
        // data inside first_page we have no undefined behavior problems
        let index_ref = structs::WalIndexHeader::ref_from_bytes(
            &self.first_page.get_mut()[0..structs::WAL_INDEX_HEADER_SIZE],
        )
        .expect("it fits");
        index_ref.info.clone()
    }
    */

    fn set_index(&mut self, info: &structs::WalIndexInfo) {
        let index_ref = structs::WalIndexHeader::mut_from_bytes(
            &mut self.first_page.get_mut()[0..structs::WAL_INDEX_HEADER_SIZE],
        )
        .expect("it fits");
        index_ref.info = info.clone();
        index_ref.info_copy = info.clone();
    }

    fn set_tail(&mut self, tail: &structs::WalIndexHeaderTail) {
        let index_ref = structs::WalIndexHeader::mut_from_bytes(
            &mut self.first_page.get_mut()[0..structs::WAL_INDEX_HEADER_SIZE],
        )
        .expect("it fits");
        index_ref.tail = tail.clone();
    }

    /*
    fn get_index_double_read(&self) -> structs::WalIndexInfo {}
        // this function does a double read to catch racing writes
        let mut info_bytes: [u8; structs::WAL_INDEX_INFO_SIZE] = [0; _];
        let mut info_bytes_copy: [u8; structs::WAL_INDEX_INFO_SIZE] = [0; _];

        // use unsafe and std::ptr::copy to force the compiler to
        // not elide reads
        unsafe {
            let header_ptr = self.first_page.get() as *mut u8;
            let index_info_ptr = header_ptr.byte_offset(offset_of!(structs::WalIndexHeader, info) as isize);

            std::ptr::copy(index_info_ptr, info_bytes.as_mut_ptr(), info_bytes.len());
            // TODO: actually I am not convinced this is safe;
            // the std::ptr documentation says that even with volatile_get
            // concurrent access is not allowed
            // TODO: mem barrier from sqlite
            std::ptr::copy(index_info_ptr, info_bytes_copy.as_mut_ptr(), info_bytes.len());
        }

        if info_bytes != info_bytes_copy {
            return ErrCtx::Busy.into()
        }

        Ok(zerocopy::transmute!(info_bytes))
    }
    */
}

#[derive(Debug, Clone)]
pub struct DbClientHandle {
    id: String,
    pub data: Arc<Mutex<DbClient>>,
}

pub trait DbServer: Clone + Send + Sync {
    async fn get_latest_snapshot(&self) -> Result<(Vec<u8>, u64), ErrCtx>;
    async fn get_changes_after(&self, version: u64) -> Result<Vec<(Transaction, u64)>, ErrCtx>; // +1, etc
    async fn commit_change(&self, version: u64, tx: &Transaction) -> Result<u64, ErrCtx>;

    // LATER: advisory lock for begin tx???
}

#[derive(Debug)]
struct TestDbServerData {
    latest_snapshot: Vec<u8>,
    latest_snapshot_version: u64,

    changes: Vec<(Transaction, u64)>,
}

#[derive(Debug, Clone)]
pub struct TestDbServer {
    handle: Arc<Mutex<TestDbServerData>>,
}

#[derive(Debug)]
pub struct RemoteDbServer {
    rt: tokio::runtime::Handle,
    client: reqwest::Client,
    server: String,
}

impl RemoteDbServer {
    pub fn new(rt: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(RemoteDbServer {
            rt: rt, // rt.handle().clone(),
            // rt: tokio::runtime::Handle::current().clone(),
            client: reqwest::Client::new(),
            server: "http://localhost:3000".into(),
        })
    }
}

impl DbServer for Arc<RemoteDbServer> {
    async fn get_latest_snapshot(&self) -> Result<(Vec<u8>, u64), ErrCtx> {
        let (db, version): (Vec<u8>, u64) = self
            .client
            .post(format!("{}/get_latest_snapshot", self.server))
            .send()
            .await
            .expect("help")
            .json()
            .await
            .expect("help");

        Ok((db, version))
    }

    async fn get_changes_after(&self, version: u64) -> Result<Vec<(Transaction, u64)>, ErrCtx> {
        let changes: Vec<(Transaction, u64)> = self
            .client
            .post(format!("{}/get_changes_after", self.server))
            .json(&version)
            .send()
            .await
            .expect("help")
            .json()
            .await
            .expect("help");

        Ok(changes)
    }

    async fn commit_change(&self, version: u64, change: &Transaction) -> Result<u64, ErrCtx> {
        let new_version: u64 = self
            .client
            .post(format!("{}/commit_change", self.server))
            .json(&(version, change))
            .send()
            .await
            .expect("help")
            .json()
            .await
            .expect("help");

        Ok(new_version)
    }
}

async fn get_latest_snapshot(server: Extension<TestDbServer>) -> Json<(Vec<u8>, u64)> {
    let (db, version) = server.get_latest_snapshot().await.expect("comeon");
    info!("returning snapshot {}", version);
    Json((db, version))
}

async fn get_changes_after(
    server: Extension<TestDbServer>,
    Json(version): Json<u64>,
) -> Json<Vec<(Transaction, u64)>> {
    let changes = server.get_changes_after(version).await.expect("comeon");
    for (_, change) in &changes {
        info!("returning change {}", change);
    }
    Json(changes)
}

async fn commit_change(
    server: Extension<TestDbServer>,
    Json((version, transaction)): Json<(u64, Transaction)>,
) -> Json<u64> {
    let new_version = server
        .commit_change(version, &transaction)
        .await
        .expect("comeon");
    info!("committed change {}", new_version);
    Json(new_version)
}

pub async fn run_web_server(server: TestDbServer) {
    let router = axum::Router::new()
        .route(
            "/get_latest_snapshot",
            axum::routing::post(get_latest_snapshot),
        )
        .route("/get_changes_after", axum::routing::post(get_changes_after))
        .route("/commit_change", axum::routing::post(commit_change))
        .layer(Extension(server));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .expect("wtf");
    // Start the server
    axum::serve(listener, router.into_make_service())
        .await
        .unwrap();
}

impl TestDbServer {
    pub fn new() -> Self {
        Self {
            handle: Arc::new(Mutex::new(TestDbServerData {
                latest_snapshot: make_initial_db(),
                latest_snapshot_version: 0,
                changes: vec![],
            })),
        }
    }
}

impl DbServer for TestDbServer {
    async fn get_latest_snapshot(&self) -> Result<(Vec<u8>, u64), ErrCtx> {
        let guard = self.handle.lock();
        Ok((guard.latest_snapshot.clone(), guard.latest_snapshot_version))
    }

    async fn get_changes_after(&self, version: u64) -> Result<Vec<(Transaction, u64)>, ErrCtx> {
        let guard = self.handle.lock();
        let mut changes = vec![];
        for (tx, tx_version) in &guard.changes {
            if tx_version > &version {
                changes.push((tx.clone(), tx_version.clone()));
            }
        }
        Ok(changes)
    }

    async fn commit_change(&self, version: u64, tx: &Transaction) -> Result<u64, ErrCtx> {
        let mut guard = self.handle.lock();
        if guard.latest_snapshot_version != version {
            return ErrCtx::Bad.into();
        }

        // TODO: if this fails, are we broken?
        apply_tx(&mut guard.latest_snapshot, tx)?;
        let new_version = version + 1;
        guard.latest_snapshot_version = new_version;

        guard.changes.push((tx.clone(), new_version));

        Ok(new_version)
    }
}

#[derive(Debug)]
pub struct DbClient {
    server: Arc<RemoteDbServer>,
    server_version: u64,

    // TODO: make this a different thing?
    db_data: Vec<u8>,
    // TODO: make this a different thing?
    wal_data: Vec<u8>,

    wal_index: WalIndex, // TODO: multiple pages Vec<u8>,
    wal_index_mapped: bool,

    write_locks: i32,
    read_locks: i32,
    had_write: bool,

    pub pending_open: Option<ThreadId>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PageWrite {
    pub page_no: u32,

    #[serde(with = "base64")]
    pub page_data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Transaction {
    pub page_writes: Vec<PageWrite>,
    pub db_size_after: u32,
}

fn apply_tx(db_data: &mut Vec<u8>, tx: &Transaction) -> Result<(), ErrCtx> {
    for page in &tx.page_writes {
        let page_offset = (page.page_no as usize - 1) * PAGESIZE;
        while page_offset + PAGESIZE > db_data.len() {
            db_data.append(&mut vec![0u8; PAGESIZE]);
        }
        db_data[page_offset..page_offset + PAGESIZE].copy_from_slice(&page.page_data);
    }
    db_data.truncate(PAGESIZE * tx.db_size_after as usize);
    Ok(())
}

impl DbClient {
    fn extract_writes_from_wal(
        &mut self,
        start: usize,
        end: usize,
    ) -> Result<Vec<PageWrite>, ErrCtx> {
        let mut page_writes = vec![];

        for idx in (start..end).step_by(structs::WAL_FRAME_HEADER_SIZE + PAGESIZE) {
            let header = structs::WalFrameHeader::ref_from_bytes(
                &self.wal_data[idx..idx + structs::WAL_FRAME_HEADER_SIZE],
            )
            .expect("bad");

            let page = &self.wal_data[idx + structs::WAL_FRAME_HEADER_SIZE
                ..idx + structs::WAL_FRAME_HEADER_SIZE + PAGESIZE];

            let page_number: u32 = header.page_number.into();

            page_writes.push(PageWrite {
                page_no: page_number,
                page_data: page.into(),
            });
        }

        Ok(page_writes)
    }

    fn apply_tx(&mut self, tx: &Transaction) -> Result<(), ErrCtx> {
        apply_tx(&mut self.db_data, tx)
    }

    async fn pull_changes(&mut self) -> Result<(), ErrCtx> {
        info!("pulling changes");
        let changes = self.server.get_changes_after(self.server_version).await?;
        for (tx, version) in &changes {
            info!("applying tx version {}", version);
            self.apply_tx(tx)?;
            self.server_version = version.clone();
        }

        if changes.len() > 0 {
            // TODO: do this here?
            self.reset_wal()?;
        }

        Ok(())
    }

    async fn maybe_begin_tx(&mut self) -> Result<(), ErrCtx> {
        if self.write_locks + self.read_locks != 0 {
            return Ok(());
        }

        self.pull_changes().await?;

        Ok(())
    }

    async fn maybe_end_tx(&mut self) -> Result<(), ErrCtx> {
        if self.write_locks + self.read_locks != 0 {
            return Ok(());
        }

        info!(?self.had_write, "unlocked both");

        if self.had_write {
            // for now, skip header (maybe later don't so we can check checksums etc?) (todo test: what happens when we abort? etc)

            let mut start_idx: usize = structs::WAL_HEADER_SIZE;

            for idx in
                (start_idx..self.wal_data.len()).step_by(structs::WAL_FRAME_HEADER_SIZE + PAGESIZE)
            {
                let header = structs::WalFrameHeader::ref_from_bytes(
                    &self.wal_data[idx..idx + structs::WAL_FRAME_HEADER_SIZE],
                )
                .expect("bad");

                let db_size_after_commit: u32 = header.database_size_after_commit.into();

                if db_size_after_commit != 0 {
                    let end_idx = idx + structs::WAL_FRAME_HEADER_SIZE + PAGESIZE;

                    let page_writes = self.extract_writes_from_wal(start_idx, end_idx)?;

                    let tx = Transaction {
                        page_writes: page_writes,
                        db_size_after: db_size_after_commit,
                    };

                    // let json = serde_json::to_string_pretty(&tx).expect("ok");
                    // println!("{}", json);

                    start_idx = end_idx;

                    // TODO: this is broken if we sent multiple transactions in a row. but that should not be allowed?
                    // TODO: can we do this commit not at the unlock path but at the write to the wal stage instead?
                    let new_version = self.server.commit_change(self.server_version, &tx).await?;
                    tracing::info!("sent tx version {}", new_version);

                    // self.pull_changes()?;

                    // self.apply_tx(&tx)?;
                    // self.server_version = new_version;
                }
            }

            self.reset_wal()?;
        }

        self.had_write = false;

        Ok(())
    }

    fn reset_wal(&mut self) -> Result<(), ErrCtx> {
        self.wal_data.truncate(structs::WAL_HEADER_SIZE);
        let wal_header =
            structs::WalHeader::mut_from_bytes(&mut self.wal_data).expect("it fits ok");

        let mut salt1: u32 = wal_header.salt1.into();
        if salt1 == 0 {
            salt1 = rng().random();
        } else {
            salt1 = salt1 + 1;
        }
        let salt2: u32 = rng().random();

        *wal_header = structs::WalHeader {
            magic_number: structs::WAL_HEADER_MAGIC_NUMBER_LITTLE_ENDIAN.into(),
            file_format: structs::WAL_FILE_FORMAT_VERSION.into(),
            page_size: (PAGESIZE as u32).into(),
            checkpoint_sequence_number: 0.into(), // TODO: change?
            salt1: salt1.into(),
            salt2: salt2.into(),
            checksum1: 0.into(),
            checksum2: 0.into(),
        };

        let dst: &[zerocopy::byteorder::little_endian::U32] =
            FromBytes::ref_from_bytes(&wal_header.as_bytes()[0..structs::WAL_HEADER_SIZE - 8])
                .expect("should fit");

        let checksum = compute_checksum(dst);

        // we compute the header using little endian but store it in the header as big endian (???)
        wal_header.checksum1 = Into::<u32>::into(checksum[0]).into();
        wal_header.checksum2 = Into::<u32>::into(checksum[1]).into();

        let mut info = structs::WalIndexInfo {
            version_number: structs::WAL_INDEX_FORMAT_VERSION.into(),
            padding: [0; 4],
            change_counter: 0.into(),
            is_init: 1,
            big_endian_checksums: 0, // see WalHeader.magic_number
            page_size: (PAGESIZE as u16).into(),
            mx_frame: 0.into(),
            n_page: ((self.db_data.len() / PAGESIZE) as u32).into(),
            last_frame_checksum1: 0.into(), // should this match the page header?
            last_frame_checksum2: 0.into(), //
            salt1: wal_header.salt1,
            salt2: wal_header.salt2,
            checksum1: 0.into(),
            checksum2: 0.into(),
        };

        let checksum = {
            let dst: &[zerocopy::byteorder::native_endian::U32] =
                FromBytes::ref_from_bytes(&info.as_bytes()[0..structs::WAL_INDEX_INFO_SIZE - 8])
                    .expect("should fit");
            compute_checksum(dst)
        };
        info.checksum1 = checksum[0];
        info.checksum2 = checksum[1];

        self.wal_index.set_index(&info);

        // TODO: do not have to rewrite all of these?
        self.wal_index.set_tail(&WalIndexHeaderTail {
            n_backfill: 0.into(),
            readmarks: [
                0.into(), // unused value
                0.into(), // READ(1)
                READMARK_NOT_USED.into(),
                READMARK_NOT_USED.into(),
                READMARK_NOT_USED.into(),
            ],
            unused_for_locks: [0; 8],
            n_backfill_attempted: 0.into(),
            unused_end: [0; 4],
        });

        // no need to actually update the hash tables, because sqlite takes care of that for us (see eg. logic in walIndexAppend, walRestartLog, walRestartHdr in wal.c)
        // for i in structs::WAL_INDEX_HEADER_SIZE..self.wal_index.len() {
        // TODO: do this better / more efficiently
        // self.wal_index[i] = 0;
        // }

        Ok(())
    }
}

fn compute_checksum<ByteOrder: zerocopy::ByteOrder>(
    x: &[zerocopy::byteorder::U32<ByteOrder>],
) -> [zerocopy::byteorder::U32<ByteOrder>; 2] {
    assert!(x.len() % 2 == 0, "input must have even length");

    let mut s: [u32; 2] = [0; 2];

    for i in (0..x.len()).step_by(2) {
        // s[0] += x[i] + s[1];
        s[0] = s[0]
            .wrapping_add(x[i].into())
            .wrapping_add(s[1].into())
            .into();
        // s[1] += x[i + 1] + s[0];
        s[1] = s[1]
            .wrapping_add(x[i + 1].into())
            .wrapping_add(s[0].into())
            .into();
    }

    [s[0].into(), s[1].into()]
}

fn make_initial_db() -> Vec<u8> {
    let mut initial_db_data = vec![0; PAGESIZE];

    let db_header =
        structs::SqliteHeader::mut_from_bytes(&mut initial_db_data[0..structs::SQLITE_HEADER_SIZE])
            .expect("it fits ok");
    *db_header = structs::SqliteHeader::empty_wal_header();

    {
        let btree_page_header = structs::BtreePageHeader::mut_from_bytes(
            &mut initial_db_data[structs::SQLITE_HEADER_SIZE
                ..structs::SQLITE_HEADER_SIZE + structs::BTREE_PAGE_HEADER_SIZE],
        )
        .expect("it fits ok");
        *btree_page_header = structs::BtreePageHeader {
            page_type: 13, // leaf data???
            first_freeblock: 0.into(),
            number_of_cells: 0.into(),
            start_cell_content_area: PAGESIZE.try_into().expect("fits into u16"),
            fragmented_free_bytes_in_content_area: 0,
        };
    }

    initial_db_data
}

impl DbClient {
    pub async fn new(server: Arc<RemoteDbServer>) -> Result<Self, ErrCtx> {
        let (db_data, db_version) = server.get_latest_snapshot().await?;

        let initial_wal_data = vec![0; structs::WAL_HEADER_SIZE];

        let mut result = Self {
            server: server,
            server_version: db_version,
            db_data: db_data,
            wal_data: initial_wal_data,
            wal_index: WalIndex::new(), // initial_wal_index_data,
            wal_index_mapped: false,

            write_locks: 0,
            read_locks: 0,
            had_write: false,

            pending_open: None,
        };

        result.reset_wal().expect("initial should work");

        Ok(result)
    }
}

impl DbClientHandle {
    pub async fn new(id: String, server: Arc<RemoteDbServer>) -> Result<Self, ErrCtx> {
        Ok(Self {
            id: id,
            data: Arc::new(Mutex::new(DbClient::new(server).await?)),
        })
    }
}

pub struct DbFile {
    pub handle: DbClientHandle,
    opts: OpenOpts,
}

impl Debug for DbFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.handle.id.to_string())
    }
}

impl DbFile {
    pub fn new(handle: DbClientHandle, opts: OpenOpts) -> Self {
        Self { handle, opts }
    }

    pub fn handle(&self) -> &DbClientHandle {
        &self.handle
    }

    pub fn opts(&self) -> OpenOpts {
        self.opts
    }
}

static ZEROES_PAGE: [u8; 4096] = [0u8; 4096];

impl VfsFile for DbFile {
    fn readonly(&self) -> bool {
        false
    }

    fn in_memory(&self) -> bool {
        false
    }

    fn lock(&mut self, level: LockLevel) -> Result<(), ErrCtx> {
        if level == LockLevel::Exclusive {
            // reject exclusive locks on the DB
            return ErrCtx::Busy.into();
        }
        Ok(())
    }

    fn unlock(&mut self, level: LockLevel) -> Result<(), ErrCtx> {
        Ok(())
    }

    fn file_size(&mut self) -> Result<usize, ErrCtx> {
        let guard = self.handle.data.lock();
        Ok(guard.db_data.len())
    }

    fn read(&mut self, offset: usize, data: &mut [u8]) -> Result<usize, ErrCtx> {
        // locate the page offset of the requested page
        let page_idx: usize = ((offset / PAGESIZE) + 1)
            .try_into()
            .expect("offset out of volume range");
        // local_offset is the offset *within* the requested page
        let local_offset = offset % PAGESIZE;

        info!(?offset, ?page_idx, "reading");

        assert!(
            local_offset + data.len() <= PAGESIZE,
            "read must not cross page boundary"
        );
        assert!(local_offset == 0, "reads only happen on page alignment");

        let offset: usize = (page_idx - 1) * PAGESIZE;
        let guard = self.handle.data.lock();

        let mut source = if offset >= guard.db_data.len() {
            &ZEROES_PAGE as &[u8]
        } else {
            &guard.db_data[offset..offset + PAGESIZE]
        };

        // TODO: partial pages don't happen right (not true, first page read can be 100 bytes for header)
        if source.len() > data.len() {
            source = &source[0..data.len()];
        }

        data.copy_from_slice(source);

        Ok(data.len())
    }

    fn truncate(&mut self, size: usize) -> Result<(), ErrCtx> {
        ErrCtx::Bad.into()
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<usize, ErrCtx> {
        ErrCtx::Bad.into()
    }

    fn shm_map(
        &mut self,
        page_idx: usize,
        page_size: usize,
        _extend: bool,
    ) -> Result<*mut c_void, ErrCtx> {
        if page_idx != 0 {
            return ErrCtx::Bad.into();
        }

        if page_size != 32768 {
            return ErrCtx::Bad.into();
        }

        let mut guard = self.handle.data.lock();

        if guard.wal_index_mapped {
            return ErrCtx::Bad.into();
        }
        guard.wal_index_mapped = true;

        Ok(guard.wal_index.void_pointer_for_sqlite())
    }

    async fn shm_lock(
        &mut self,
        lock_idx: flags::ShmLockIndex,
        _n: usize,
        lock_type: flags::ShmLockMode,
        lock_op: flags::ShmLockOp,
    ) -> Result<(), ErrCtx> {
        let mut guard = self.handle.data.lock();

        match lock_idx {
            flags::ShmLockIndex::Write => {
                if lock_op == flags::ShmLockOp::Lock {
                    if lock_type == flags::ShmLockMode::Shared {
                        ErrCtx::Busy.into()
                    } else {
                        // TODO: should this be possible?
                        guard.maybe_begin_tx().await?;

                        // TODO: check that it is 0?
                        guard.write_locks += 1;
                        guard.had_write = true;
                        info!("started write");
                        Ok(())
                    }
                } else {
                    if lock_type == flags::ShmLockMode::Shared {
                        ErrCtx::Bad.into()
                    } else {
                        // TODO: check that it is 1?
                        guard.write_locks -= 1;
                        guard.maybe_end_tx().await?;
                        Ok(())
                    }
                }
            }
            flags::ShmLockIndex::Recover => ErrCtx::Busy.into(),
            flags::ShmLockIndex::Read(_) => {
                if lock_op == flags::ShmLockOp::Lock {
                    if lock_type == flags::ShmLockMode::Exclusive {
                        ErrCtx::Busy.into()
                    } else {
                        if guard.read_locks == 0 {
                            info!("started read");
                        }
                        // TODO: should this be possible?
                        guard.maybe_begin_tx().await?;
                        guard.read_locks += 1;
                        Ok(())
                    }
                } else {
                    if lock_type == flags::ShmLockMode::Exclusive {
                        ErrCtx::Bad.into()
                    } else {
                        guard.read_locks -= 1;
                        guard.maybe_end_tx().await?;
                        Ok(())
                    }
                }
            }
            flags::ShmLockIndex::Checkpoint => ErrCtx::Busy.into(),
        }
        // Ok(())
    }

    fn shm_barrier(&mut self) {}

    fn shm_unmap(&mut self, _delete: bool) -> Result<(), ErrCtx> {
        let mut guard = self.handle.data.lock();
        if !guard.wal_index_mapped {
            return ErrCtx::Bad.into();
        }
        guard.wal_index_mapped = false;
        Ok(())
    }
}

pub struct WalFile {
    handle: DbClientHandle,
    opts: OpenOpts,
}

impl WalFile {
    pub fn new(handle: DbClientHandle, opts: OpenOpts) -> Self {
        Self { handle, opts }
    }
}

impl Debug for WalFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.handle.id.to_string())
    }
}

impl VfsFile for WalFile {
    fn readonly(&self) -> bool {
        false
    }

    fn in_memory(&self) -> bool {
        false
    }

    fn lock(&mut self, _level: LockLevel) -> Result<(), ErrCtx> {
        Ok(())
    }

    fn unlock(&mut self, _level: LockLevel) -> Result<(), ErrCtx> {
        Ok(())
    }

    fn file_size(&mut self) -> Result<usize, ErrCtx> {
        let guard = self.handle.data.lock();
        Ok(guard.wal_data.len())
    }

    fn read(&mut self, offset: usize, data: &mut [u8]) -> Result<usize, ErrCtx> {
        let guard = self.handle.data.lock();
        let start = offset.min(guard.wal_data.len());
        let end = (offset + data.len()).min(guard.wal_data.len());
        let len = end - start;
        data[0..len].copy_from_slice(&guard.wal_data[start..end]);
        Ok(end - start)
    }

    fn truncate(&mut self, size: usize) -> Result<(), ErrCtx> {
        let mut guard = self.handle.data.lock();
        guard.wal_data.truncate(size);
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<usize, ErrCtx> {
        let mut guard = self.handle.data.lock();

        if offset + data.len() > guard.wal_data.len() {
            guard.wal_data.resize(offset + data.len(), 0);
        }
        guard.wal_data[offset..offset + data.len()].copy_from_slice(data);

        if data.len() == structs::WAL_HEADER_SIZE {
            let wal_header = structs::WalHeader::ref_from_bytes(data).expect("checked size");
            tracing::info!("got wal header {:?}", wal_header);
        }

        if data.len() == structs::WAL_FRAME_HEADER_SIZE {
            let wal_frame_header =
                structs::WalFrameHeader::ref_from_bytes(data).expect("checked size");
            tracing::info!("got wal frame header {:?}", wal_frame_header);
        }

        if data.len() == PAGESIZE {
            tracing::info!("got wal frame page");
        }

        Ok(data.len())
    }

    fn shm_map(
        &mut self,
        _page_idx: usize,
        _page_size: usize,
        _extend: bool,
    ) -> Result<*mut c_void, ErrCtx> {
        ErrCtx::Bad.into()
    }

    async fn shm_lock(
        &mut self,
        _lock_idx: flags::ShmLockIndex,
        _n: usize,
        _lock_type: flags::ShmLockMode,
        _lock_op: flags::ShmLockOp,
    ) -> Result<(), ErrCtx> {
        ErrCtx::Bad.into()
    }

    fn shm_barrier(&mut self) {}

    fn shm_unmap(&mut self, _delete: bool) -> Result<(), ErrCtx> {
        ErrCtx::Bad.into()
    }
}

mod base64 {
    use serde::{Deserialize, Serialize};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        let base64 = base64::encode(v);
        String::serialize(&base64, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let base64 = String::deserialize(d)?;
        base64::decode(base64.as_bytes()).map_err(|e| serde::de::Error::custom(e))
    }
}
