use std::ffi::c_void;
use std::fmt::Debug;

use culprit::Result;
use sqlite_plugin::flags;
use sqlite_plugin::flags::LockLevel;

use super::ErrCtx;

use super::VfsFile;

#[derive(Default)]
pub struct MemFile {
    pub name: String,
    pub data: Vec<u8>,
}

impl Debug for MemFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
        // f.debug_struct("MemFile")
        // .field("len", &self.data.len())
        // .finish()
    }
}

impl VfsFile for MemFile {
    fn readonly(&self) -> bool {
        false
    }

    fn in_memory(&self) -> bool {
        true
    }

    fn lock(&mut self, _level: LockLevel) -> Result<(), ErrCtx> {
        Ok(())
    }

    fn unlock(&mut self, _level: LockLevel) -> Result<(), ErrCtx> {
        Ok(())
    }

    fn file_size(&mut self) -> Result<usize, ErrCtx> {
        Ok(self.data.len())
    }

    fn truncate(&mut self, size: usize) -> Result<(), ErrCtx> {
        self.data.truncate(size);
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<usize, ErrCtx> {
        if offset + data.len() > self.data.len() {
            self.data.resize(offset + data.len(), 0);
        }
        self.data[offset..offset + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn read(&mut self, offset: usize, data: &mut [u8]) -> Result<usize, ErrCtx> {
        let start = offset.min(self.data.len());
        let end = (offset + data.len()).min(self.data.len());
        let len = end - start;
        data[0..len].copy_from_slice(&self.data[start..end]);
        Ok(end - start)
    }

    fn shm_map(
        &mut self,
        _page_idx: usize,
        _page_size: usize,
        _extend: bool,
    ) -> Result<*mut c_void, ErrCtx> {
        todo!("missing")
    }

    async fn shm_lock(
        &mut self,
        _lock_idx: flags::ShmLockIndex,
        _n: usize,
        _lock_type: flags::ShmLockMode,
        _lock_op: flags::ShmLockOp,
    ) -> Result<(), ErrCtx> {
        todo!("missing")
    }

    fn shm_barrier(&mut self) {
        todo!("missing")
    }

    fn shm_unmap(&mut self, _delete: bool) -> Result<(), ErrCtx> {
        todo!("missing")
    }
}
