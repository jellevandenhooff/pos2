use anyhow::{Result, anyhow};
use std::{
    ffi::{CString, c_void},
    os::raw::c_char,
};
use tracing::level_filters::LevelFilter;

use sqlite_plugin::{
    flags::{AccessFlags, LockLevel, OpenOpts},
    logger::{SqliteLogLevel, SqliteLogger},
    sqlite3_api_routines, vars,
    vfs::{Pragma, PragmaErr, RegisterOpts, Vfs, VfsHandle, VfsResult, register_dynamic},
};

pub mod replvfs;

/// This function is called by `SQLite` when the extension is loaded. It registers
/// the memvfs VFS with `SQLite`.
/// # Safety
/// This function should only be called by sqlite's extension loading mechanism.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlite3_extension_init(
    _db: *mut c_void,
    _pz_err_msg: *mut *mut c_char,
    p_api: *mut sqlite3_api_routines,
) -> std::os::raw::c_int {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    tracing::info!("hello world");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let vfs_name = "replvfs";
    let vfs = crate::replvfs::ReplVfs::new(replvfs::Tokio::Runtime(runtime));

    if let Err(err) = unsafe {
        register_dynamic(
            p_api,
            CString::new(vfs_name).unwrap(),
            vfs,
            RegisterOpts { make_default: true },
        )
    } {
        return err;
    }

    /*
                0
            })
            .await;

            res
        })
        .expect("ok");

    if res != 0 {
        return res;
    }
    */

    // set the log level to trace
    // log::set_max_level(log::LevelFilter::Trace);

    vars::SQLITE_OK_LOAD_PERMANENTLY
}
