use std::ffi::c_void;
use std::fmt::Debug;

use super::mem_file::MemFile;
use super::vol_file::DbFile;
use super::vol_file::WalFile;
use culprit::Result;
use enum_dispatch::enum_dispatch;
use sqlite_plugin::flags;
use sqlite_plugin::{flags::LockLevel, vfs::VfsHandle};

use super::ErrCtx;

#[enum_dispatch]
pub trait VfsFile: Debug {
    fn readonly(&self) -> bool;
    fn in_memory(&self) -> bool;

    fn lock(&mut self, level: LockLevel) -> Result<(), ErrCtx>;
    fn unlock(&mut self, level: LockLevel) -> Result<(), ErrCtx>;

    fn file_size(&mut self) -> Result<usize, ErrCtx>;
    fn truncate(&mut self, size: usize) -> Result<(), ErrCtx>;

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<usize, ErrCtx>;
    fn read(&mut self, offset: usize, data: &mut [u8]) -> Result<usize, ErrCtx>;

    fn shm_map(
        &mut self,
        page_idx: usize,
        page_size: usize,
        extend: bool,
    ) -> Result<*mut c_void, ErrCtx>;

    fn shm_lock(
        &mut self,
        lock_idx: flags::ShmLockIndex,
        n: usize,
        lock_type: flags::ShmLockMode,
        lock_op: flags::ShmLockOp,
    ) -> Result<(), ErrCtx>;

    fn shm_barrier(&mut self);

    fn shm_unmap(&mut self, delete: bool) -> Result<(), ErrCtx>;
}

#[enum_dispatch(VfsFile)]
#[derive(Debug)]
pub enum FileHandle {
    MemFile,
    DbFile,
    WalFile,
}

impl VfsHandle for FileHandle {
    fn readonly(&self) -> bool {
        VfsFile::readonly(self)
    }

    fn in_memory(&self) -> bool {
        VfsFile::in_memory(self)
    }
}
