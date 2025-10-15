use anyhow::{Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use std::ffi::CString;
use std::{io::Write, println};
use tracing::info;

use sqlite_plugin::vfs::{RegisterOpts, Vfs};

use crate::replvfs::vol_file::{TestDbServer, run_web_server};

pub mod replvfs;

fn log_handler(_: i32, arg2: &str) {
    println!("sqlite log: {arg2}");
}

#[tokio::main]
async fn main() -> Result<()> {
    unsafe {
        rusqlite::trace::config_log(Some(log_handler)).unwrap();
    }

    tracing_subscriber::fmt::init();

    let server = TestDbServer::new();
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).is_some_and(|s| s == "server") {
        tokio::task::spawn(async move {
            run_web_server(server).await;
        })
        .await?;
    }

    if args.get(1).is_some_and(|s| s == "client") {
        tokio::task::spawn_blocking(|| -> Result<()> {
            let vfs_name = "replvfs";
            let vfs = replvfs::ReplVfs::new();

            sqlite_plugin::vfs::register_static(
                CString::new(vfs_name).unwrap(),
                vfs,
                RegisterOpts { make_default: true },
            )
            .map_err(|_| anyhow!("failed to register vfs"))?;

            // create a sqlite connection using the mock vfs
            let conn1 = Connection::open_with_flags_and_vfs(
                "main.db",
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
                vfs_name,
                // "memvfs",
            )?;

            let conn2 = Connection::open_with_flags_and_vfs(
                "main.db",
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
                vfs_name,
                // "memvfs",
            )?;

            // info!("enabling wal");
            // let journal_mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
            // if journal_mode != "wal" {
            // error!(?journal_mode, "could not enable wal");
            // bail!("help");
            // }
            // info!("enabled wal");

            info!("create table");
            conn1.execute("create table t (val int)", [])?;

            info!("insert 1");
            conn1.execute("insert into t (val) values (1)", [])?;

            info!("insert 2");
            conn1.execute("insert into t (val) values (2)", [])?;

            info!("pragma mock_test");
            conn1.execute("pragma mock_test", [])?;

            info!("select");
            let n: i64 = conn1.query_row("select sum(val) from t", [], |row| row.get(0))?;
            info!(n, "select");
            assert_eq!(n, 3);

            info!("create blob table");
            // the blob api is interesting and stress tests reading/writing pages and journaling
            conn1.execute("create table b (data blob)", [])?;

            {
                info!("inserting zero blob");
                conn1.execute("insert into b values (zeroblob(8192))", [])?;
                let rowid = conn1.last_insert_rowid();
                let mut blob = conn1.blob_open(rusqlite::MAIN_DB, "b", "data", rowid, false)?;

                // write some data to the blob
                info!("writing to blob");
                let n = blob.write(b"hello")?;
                assert_eq!(n, 5);
            }

            info!("query blob");
            // query the table for the blob and print it
            let mut stmt = conn1.prepare("select data from b")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let data: Vec<u8> = row.get(0)?;
                assert_eq!(&data[0..5], b"hello");
            }

            info!("conn2 select");
            let n: i64 = conn2.query_row("select sum(val) from t", [], |row| row.get(0))?;
            info!(n, "select");
            assert_eq!(n, 3);

            info!("conn1 insert 3");
            conn1.execute("insert into t (val) values (3)", [])?;

            info!("conn2 select");
            let n: i64 = conn2.query_row("select sum(val) from t", [], |row| row.get(0))?;
            info!(n, "select");
            assert_eq!(n, 6);

            info!("close db");

            Ok(())
        })
        .await??;
    }

    Ok(())
}

// todo:
// - tokio driver / integrate with vfs
// - unit tests
// - error handling instead of expect
// - open multiple databases (pass name?)
// - something less brittle than version numbers (uuid?)
// - handle failed commit / conflict
// - store db (or transactions?)
