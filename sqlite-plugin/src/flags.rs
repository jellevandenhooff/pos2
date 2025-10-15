use core::fmt::{self, Debug, Formatter};

use crate::vars;

#[derive(Debug, PartialEq, Eq)]
pub enum OpenKind {
    Unknown,
    MainDb,
    MainJournal,
    TempDb,
    TempJournal,
    TransientDb,
    SubJournal,
    SuperJournal,
    Wal,
}

impl OpenKind {
    pub fn is_temp(&self) -> bool {
        matches!(self, Self::TempDb | Self::TempJournal | Self::TransientDb)
    }
}

impl From<i32> for OpenKind {
    fn from(flags: i32) -> Self {
        match flags {
            flags if flags & vars::SQLITE_OPEN_MAIN_DB > 0 => Self::MainDb,
            flags if flags & vars::SQLITE_OPEN_MAIN_JOURNAL > 0 => Self::MainJournal,
            flags if flags & vars::SQLITE_OPEN_TEMP_DB > 0 => Self::TempDb,
            flags if flags & vars::SQLITE_OPEN_TEMP_JOURNAL > 0 => Self::TempJournal,
            flags if flags & vars::SQLITE_OPEN_TRANSIENT_DB > 0 => Self::TransientDb,
            flags if flags & vars::SQLITE_OPEN_SUBJOURNAL > 0 => Self::SubJournal,
            flags if flags & vars::SQLITE_OPEN_SUPER_JOURNAL > 0 => Self::SuperJournal,
            flags if flags & vars::SQLITE_OPEN_WAL > 0 => Self::Wal,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateMode {
    None,
    Create,
    MustCreate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OpenMode {
    ReadOnly,
    ReadWrite { create: CreateMode },
}

impl From<i32> for OpenMode {
    fn from(flags: i32) -> Self {
        const MUST_CREATE: i32 = vars::SQLITE_OPEN_CREATE | vars::SQLITE_OPEN_EXCLUSIVE;
        match flags {
            flags if flags & vars::SQLITE_OPEN_READONLY > 0 => Self::ReadOnly,
            flags if flags & vars::SQLITE_OPEN_READWRITE > 0 => Self::ReadWrite {
                create: match flags {
                    flags if flags & MUST_CREATE == MUST_CREATE => CreateMode::MustCreate,
                    flags if flags & vars::SQLITE_OPEN_CREATE > 0 => CreateMode::Create,
                    _ => CreateMode::None,
                },
            },
            _ => Self::ReadOnly,
        }
    }
}

impl OpenMode {
    pub fn must_create(&self) -> bool {
        matches!(self, Self::ReadWrite { create: CreateMode::MustCreate })
    }
    pub fn is_readonly(&self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

#[derive(Clone, Copy)]
pub struct OpenOpts {
    flags: i32,
}

impl OpenOpts {
    pub fn new(flags: i32) -> Self {
        Self { flags }
    }

    pub fn flags(&self) -> i32 {
        self.flags
    }

    pub fn kind(&self) -> OpenKind {
        self.flags.into()
    }

    pub fn mode(&self) -> OpenMode {
        self.flags.into()
    }

    pub fn delete_on_close(&self) -> bool {
        self.flags & vars::SQLITE_OPEN_DELETEONCLOSE > 0
    }

    pub fn set_readonly(&mut self) {
        self.flags &= !vars::SQLITE_OPEN_READWRITE;
        self.flags |= vars::SQLITE_OPEN_READONLY;
    }
}

impl From<i32> for OpenOpts {
    fn from(flags: i32) -> Self {
        Self::new(flags)
    }
}

impl Debug for OpenOpts {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenOpts")
            .field("flags", &self.flags)
            .field("kind", &self.kind())
            .field("mode", &self.mode())
            .field("delete_on_close", &self.delete_on_close())
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AccessFlags {
    Exists,
    Read,
    ReadWrite,
}

impl From<i32> for AccessFlags {
    fn from(flags: i32) -> Self {
        match flags {
            flags if flags == vars::SQLITE_ACCESS_EXISTS => Self::Exists,
            flags if flags & vars::SQLITE_ACCESS_READ > 0 => Self::Read,
            flags if flags & vars::SQLITE_ACCESS_READWRITE > 0 => Self::ReadWrite,
            _ => Self::Exists,
        }
    }
}

/// Represents one of the 5 `SQLite` locking levels.
/// See [SQLite documentation](https://www.sqlite.org/lockingv3.html) for more information.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum LockLevel {
    /// No locks are held; the database may be neither read nor written.
    Unlocked,

    /// The database may be read but not written. Multiple Shared locks can
    /// coexist at once.
    Shared,

    /// A shared lock with the intention to upgrade to an exclusive lock. Only
    /// one Reserved lock can exist at once.
    Reserved,

    /// A lock in the process of upgrading to a reserved lock. Can coexist with
    /// Shared locks, but no new shared locks can be taken.
    Pending,

    /// The database may be read or written, but no other locks can be held.
    Exclusive,
}

impl From<i32> for LockLevel {
    fn from(lock: i32) -> Self {
        match lock {
            vars::SQLITE_LOCK_NONE => Self::Unlocked,
            vars::SQLITE_LOCK_SHARED => Self::Shared,
            vars::SQLITE_LOCK_RESERVED => Self::Reserved,
            vars::SQLITE_LOCK_PENDING => Self::Pending,
            vars::SQLITE_LOCK_EXCLUSIVE => Self::Exclusive,
            _ => panic!("invalid lock level: {}", lock),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum ShmLockMode {
    Shared,
    Exclusive,
}

impl From<i32> for ShmLockMode {
    fn from(flags: i32) -> Self {
        match flags {
            flags if flags & vars::SQLITE_SHM_SHARED > 0 => Self::Shared,
            flags if flags & vars::SQLITE_SHM_EXCLUSIVE > 0 => Self::Exclusive,
            _ => panic!("invalid shm lock type: {}", flags),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum ShmLockOp {
    Lock,
    Unlock,
}

impl From<i32> for ShmLockOp {
    fn from(flags: i32) -> Self {
        match flags {
            flags if flags & vars::SQLITE_SHM_LOCK > 0 => Self::Lock,
            flags if flags & vars::SQLITE_SHM_UNLOCK > 0 => Self::Unlock,
            _ => panic!("invalid shm lock op: {}", flags),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum ShmLockIndex {
    Write,
    Checkpoint,
    Recover,
    Read(i32),
}

impl From<i32> for ShmLockIndex {
    fn from(lock: i32) -> Self {
        match lock {
            0 => ShmLockIndex::Write,
            1 => ShmLockIndex::Checkpoint,
            2 => ShmLockIndex::Recover,
            n if n >= 3 && n < vars::SQLITE_SHM_NLOCK => ShmLockIndex::Read(n - 3),
            _ => panic!("invalid shm lock index: {}", lock),
        }
    }
}

impl Into<i32> for ShmLockIndex {
    fn into(self) -> i32 {
        match self {
            ShmLockIndex::Write => 0,
            ShmLockIndex::Checkpoint => 1,
            ShmLockIndex::Recover => 2,
            ShmLockIndex::Read(n) => 3 + n,
        }
    }
}
