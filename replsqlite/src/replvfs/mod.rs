pub mod file;
pub mod mem_file;
pub mod structs;
pub mod vol_file;

use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::c_void,
    fmt::Debug,
    sync::Arc,
};

use culprit::Culprit;
use parking_lot::Mutex;
use sqlite_plugin::{
    flags::{self, AccessFlags, LockLevel, OpenKind, OpenOpts},
    logger::SqliteLogger,
    vars::{
        self, SQLITE_BUSY, SQLITE_BUSY_SNAPSHOT, SQLITE_CANTOPEN, SQLITE_INTERNAL, SQLITE_NOTFOUND,
    },
    vfs::{Pragma, PragmaErr, SqliteErr, Vfs, VfsResult},
};
use thiserror::Error;

use file::{FileHandle, VfsFile};
use mem_file::MemFile;
use vol_file::DbFile;

use crate::replvfs::vol_file::{DbClientHandle, RemoteDbServer, TestDbServer, WalFile};

#[derive(Debug, Error)]
pub enum ErrCtx {
    // #[error("Graft client error: {0}")]
    // Client(#[from] ClientErr),
    //
    //
    #[error("Bad")]
    Bad,

    // #[error("Failed to parse VolumeId: {0}")]
    // GidParseErr(#[from] GidParseErr),
    #[error("Unknown Pragma")]
    UnknownPragma,

    #[error("Cant open Volume")]
    CantOpen,

    #[error("Transaction is busy")]
    Busy,

    #[error("The transaction snapshot is no longer current")]
    BusySnapshot,

    #[error("Invalid lock transition")]
    InvalidLockTransition,

    #[error("Invalid volume state")]
    InvalidVolumeState,

    #[error(transparent)]
    FmtErr(#[from] std::fmt::Error),
}

impl ErrCtx {
    #[inline]
    fn wrap<T>(cb: impl FnOnce() -> culprit::Result<T, ErrCtx>) -> VfsResult<T> {
        match cb() {
            Ok(t) => Ok(t),
            Err(err) => {
                let code = err.ctx().sqlite_err();
                if code == SQLITE_INTERNAL {
                    tracing::error!("{}", err);
                }
                Err(code)
            }
        }
    }

    fn sqlite_err(&self) -> SqliteErr {
        match self {
            ErrCtx::UnknownPragma => SQLITE_NOTFOUND,
            ErrCtx::CantOpen => SQLITE_CANTOPEN,
            ErrCtx::Busy => SQLITE_BUSY,
            ErrCtx::BusySnapshot => SQLITE_BUSY_SNAPSHOT,
            // ErrCtx::Client(err) => Self::map_client_err(err),
            _ => SQLITE_INTERNAL,
        }
    }

    /*
    fn map_client_err(err: &ClientErr) -> SqliteErr {
        match err {
            ClientErr::GraftErr(err) => {
                if err.code().is_client() {
                    SQLITE_INTERNAL
                } else {
                    SQLITE_IOERR
                }
            }
            ClientErr::HttpErr(_) => SQLITE_IOERR,
            ClientErr::StorageErr(store_err) => match store_err {
                StorageErr::ConcurrentWrite => SQLITE_BUSY_SNAPSHOT,
                StorageErr::FjallErr(err) => match Self::extract_ioerr(err) {
                    Some(_) => SQLITE_IOERR,
                    None => SQLITE_INTERNAL,
                },
                StorageErr::IoErr(err) => SQLITE_IOERR,
                _ => SQLITE_INTERNAL,
            },
            ClientErr::IoErr(kind) => SQLITE_IOERR,
            _ => SQLITE_INTERNAL,
        }
    }
    */

    fn extract_ioerr<'a>(
        mut err: &'a (dyn std::error::Error + 'static),
    ) -> Option<&'a std::io::Error> {
        while let Some(source) = err.source() {
            err = source;
        }
        err.downcast_ref::<std::io::Error>()
    }
}

impl<T> From<ErrCtx> for culprit::Result<T, ErrCtx> {
    fn from(err: ErrCtx) -> culprit::Result<T, ErrCtx> {
        Err(Culprit::new(err))
    }
}

fn random_string() -> String {
    let id = uuid::Uuid::new_v4();
    id.hyphenated().to_string()
}

pub struct ReplVfs {
    servers: Mutex<HashMap<String, Arc<RemoteDbServer>>>,

    pending_opens: Mutex<HashMap<std::thread::ThreadId, DbClientHandle>>,
}

impl ReplVfs {
    pub fn new(/* runtime: Runtime */) -> Self {
        Self {
            // runtime,
            servers: Default::default(),
            pending_opens: Default::default(),
        }
    }
}

impl Vfs for ReplVfs {
    type Handle = FileHandle;

    fn device_characteristics(&self) -> i32 {
        // writes up to a single page are atomic
        vars::SQLITE_IOCAP_ATOMIC512 |
        vars::SQLITE_IOCAP_ATOMIC1K |
        vars::SQLITE_IOCAP_ATOMIC2K |
        vars::SQLITE_IOCAP_ATOMIC4K |
        // after reboot following a crash or power loss, the only bytes in a file that were written
        // at the application level might have changed and that adjacent bytes, even bytes within
        // the same sector are guaranteed to be unchanged
        vars::SQLITE_IOCAP_POWERSAFE_OVERWRITE |
        // when data is appended to a file, the data is appended first then the size of the file is
        // extended, never the other way around
        vars::SQLITE_IOCAP_SAFE_APPEND |
        // information is written to disk in the same order as calls to xWrite()
        vars::SQLITE_IOCAP_SEQUENTIAL
    }

    fn register_logger(&self, _logger: SqliteLogger) {
        /*
        #[derive(Clone)]
        struct Writer(Arc<Mutex<SqliteLogger>>);

        impl std::io::Write for Writer {
            #[inline]
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0.lock().log(SqliteLogLevel::Notice, data);
                Ok(data.len())
            }

            #[inline]
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let writer = Writer(Arc::new(Mutex::new(logger)));
        let make_writer = move || writer.clone();
        graft_tracing::init_tracing_with_writer(
            TracingConsumer::Tool,
            Some(self.runtime.cid().short()),
            make_writer,
        );
        */
    }

    fn canonical_path<'a>(
        &self,
        path: std::borrow::Cow<'a, str>,
    ) -> VfsResult<std::borrow::Cow<'a, str>> {
        if path == "random" {
            Ok(random_string().into())
        } else {
            Ok(path)
        }
    }

    fn pragma(
        &self,
        handle: &mut Self::Handle,
        pragma: Pragma<'_>,
    ) -> Result<Option<String>, PragmaErr> {
        tracing::trace!("pragma: file={handle:?}, pragma={pragma:?}");
        /*
        if let FileHandle::VolFile(file) = handle {
            match GraftPragma::try_from(&pragma)?.eval(&self.runtime, file) {
                Ok(val) => Ok(val),
                Err(err) => Err(PragmaErr::Fail(
                    err.ctx().sqlite_err(),
                    Some(format!("{err:?}")),
                )),
            }
        } else {
        */
        Err(PragmaErr::NotFound)
        // }
    }

    fn access(&self, path: &str, flags: AccessFlags) -> VfsResult<bool> {
        tracing::trace!("access: path={path:?}; flags={flags:?}");
        ErrCtx::wrap(move || {
            // TODO: this is wrong... make it good (eg it does not know about -wal and rollback somehow)

            if let Some(_path) = path.strip_suffix("-journal") {
                return Ok(false);
            }

            Ok(true)

            /*
            if let Some(path) = path.strip_suffix("-wal") {
                let exists = self.volumes.lock().contains_key(path);
                println!("path = {path} exists = {exists}");
                return Ok(exists);
            }

            Ok(self.volumes.lock().contains_key(path))
            */
        })
    }

    fn open(&self, path: Option<&str>, opts: OpenOpts) -> VfsResult<Self::Handle> {
        tracing::trace!("open: path={path:?}, opts={opts:?}");
        ErrCtx::wrap(move || {
            // TODO:
            // - have (one?) shared struct for each database that holds state
            // - have walfile and dbfile both point to that shared struct
            //   - then, we can have state machine(s) that interprets reads
            //     and writes from the client
            // - to start, maybe we construct ourselves a reasonable
            //   empty first page for the db (and wal?)
            //
            // QUESTION:
            // - if there are multiple connections from the process, can we
            //   somehow keep the DB and WAL structs correlated? does it matter?
            //   - hmmm... I think we can hold any future opens after the first
            //     open until the wal is also opened (???)
            //   - actually, I think the synchronization moment is after a shared
            //     lock is grabbed on the main db.

            // we only open a Volume for main database files named after a Volume ID
            if opts.kind() == OpenKind::MainDb
                && let Some(path) = path
            {
                let vid: String = path.into();
                let server_handle = match self.servers.lock().entry(vid.clone()) {
                    Entry::Occupied(entry) => entry.get().clone(),
                    Entry::Vacant(entry) => {
                        let handle = RemoteDbServer::new();
                        entry.insert(handle.clone());
                        handle
                    }
                };
                let handle = DbClientHandle::new(vid, server_handle)?;
                return Ok(DbFile::new(handle, opts).into());
            }

            if opts.kind() == OpenKind::Wal
                && let Some(path) = path
            {
                let Some(_path) = path.strip_suffix("-wal") else {
                    return ErrCtx::Bad.into();
                };

                // let vid: String = path.into();

                let mut map_guard = self.pending_opens.lock();
                let id = std::thread::current().id();
                let handle = map_guard.remove(&id).expect("should have ready db");
                handle.data.lock().pending_open = None;

                // TODO: check that vid matches, somehow?

                // let handle = match self.volumes.lock().get(&vid) {
                // Some(value) => value.clone(),
                // None => return ErrCtx::Bad.into(),
                // };

                return Ok(WalFile::new(handle, opts).into());
            }

            // all other files use in-memory storage
            Ok(MemFile {
                name: path.unwrap_or("no path").into(),
                data: vec![],
            }
            .into())
        })
    }

    fn close(&self, handle: Self::Handle) -> VfsResult<()> {
        tracing::trace!("close: file={handle:?}");
        ErrCtx::wrap(move || {
            match handle {
                FileHandle::MemFile(_) => Ok(()),
                FileHandle::WalFile(_) => Ok(()),
                FileHandle::DbFile(vol_file) => {
                    if vol_file.opts().delete_on_close() {
                        // TODO: do we want to actually delete volumes? or mark them for deletion?
                        // self.runtime
                        // .update_volume_config(vol_file.vid(), |conf| {
                        // conf.with_sync(SyncDirection::Disabled)
                        // })
                        // .or_into_ctx()?;
                    }

                    {
                        let guard = vol_file.handle.data.lock();
                        if guard.pending_open.is_some() {
                            panic!("huh closing while opening");
                        }
                    }

                    // close and drop the vol_file
                    // TODO: does this handle now get dropped? what happens?
                    // let handle = vol_file.close();

                    Ok(())
                }
            }
        })
    }

    fn delete(&self, path: &str) -> VfsResult<()> {
        tracing::trace!("delete: path={path:?}");
        ErrCtx::wrap(|| {
            // if let Ok(vid) = path.parse() {
            // TODO: do we want to actually delete volumes? or mark them for deletion?
            // self.runtime
            // .update_volume_config(&vid, |conf| conf.with_sync(SyncDirection::Disabled))
            // .or_into_ctx()?;
            // }
            Ok(())
        })
    }

    fn lock(&self, handle: &mut Self::Handle, level: LockLevel) -> VfsResult<()> {
        tracing::trace!("lock: file={handle:?}, level={level:?}");
        if level == LockLevel::Shared
            && let FileHandle::DbFile(handle) = handle
        {
            tracing::trace!("preparing for wal open...");

            let mut map_guard = self.pending_opens.lock();
            let mut db_guard = handle.handle.data.lock();

            let id = std::thread::current().id();

            if db_guard.pending_open.is_some() {
                panic!("double pending open");
            }
            if map_guard.contains_key(&id) {
                panic!("help");
            }

            db_guard.pending_open = Some(id.clone());
            map_guard.insert(id, handle.handle.clone());
        }
        ErrCtx::wrap(move || handle.lock(level))
    }

    fn unlock(&self, handle: &mut Self::Handle, level: LockLevel) -> VfsResult<()> {
        tracing::trace!("unlock: file={handle:?}, level={level:?}");
        ErrCtx::wrap(move || handle.unlock(level))
    }

    fn file_size(&self, handle: &mut Self::Handle) -> VfsResult<usize> {
        tracing::trace!("file_size: handle={handle:?}");
        ErrCtx::wrap(move || handle.file_size())
    }

    fn truncate(&self, handle: &mut Self::Handle, size: usize) -> VfsResult<()> {
        tracing::trace!("truncate: handle={handle:?}, size={size}");
        ErrCtx::wrap(move || handle.truncate(size))
    }

    fn write(&self, handle: &mut Self::Handle, offset: usize, data: &[u8]) -> VfsResult<usize> {
        tracing::trace!(
            "write: handle={handle:?}, offset={offset}, len={}",
            data.len()
        );
        ErrCtx::wrap(move || handle.write(offset, data))
    }

    fn read(&self, handle: &mut Self::Handle, offset: usize, data: &mut [u8]) -> VfsResult<usize> {
        tracing::trace!(
            "read: handle={handle:?}, offset={offset}, len={}",
            data.len()
        );
        ErrCtx::wrap(move || handle.read(offset, data))
    }

    fn shm_map(
        &self,
        handle: &mut Self::Handle,
        page_idx: usize,
        page_size: usize,
        extend: bool,
    ) -> VfsResult<*mut c_void> {
        tracing::trace!(
            "shm map: handle={handle:?}, page_idx={page_idx}, page_size={page_size} extend={extend}",
        );
        ErrCtx::wrap(move || handle.shm_map(page_idx, page_size, extend))
    }

    fn shm_lock(
        &self,
        handle: &mut Self::Handle,
        lock_idx: flags::ShmLockIndex,
        n: usize,
        lock_type: flags::ShmLockMode,
        lock_op: flags::ShmLockOp,
    ) -> VfsResult<()> {
        tracing::trace!(
            "shm lock: handle={handle:?}, lock_idx={lock_idx:?}, n={n} lock_type={lock_type:?} lock_op={lock_op:?}",
        );
        let result = ErrCtx::wrap(move || handle.shm_lock(lock_idx, n, lock_type, lock_op));
        tracing::trace!("shm lock: result={result:?}",);
        result
    }

    fn shm_barrier(&self, handle: &mut Self::Handle) {
        tracing::trace!("shm barrier: handle={handle:?}",);
        handle.shm_barrier()
    }

    fn shm_unmap(&self, handle: &mut Self::Handle, delete: bool) -> VfsResult<()> {
        tracing::trace!("shm unmap: handle={handle:?} delete={delete}");
        ErrCtx::wrap(move || handle.shm_unmap(delete))
    }
}
