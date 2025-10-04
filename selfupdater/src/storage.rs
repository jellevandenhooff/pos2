use anyhow::{Context, Result, anyhow, bail};
use bollard::{Docker, models::*, query_parameters::*};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::{State, diff_states};

pub(crate) struct Storage {
    pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
}

impl Storage {
    pub(crate) fn init() -> Result<Storage> {
        // TODO: wal? other flags?
        let manager = r2d2_sqlite::SqliteConnectionManager::file("/data/selfupdater.sqlite3");
        let pool = r2d2::Pool::new(manager)?;
        //
        let mut conn = pool.get()?;

        let tx = conn.transaction()?;

        if !tx.table_exists(Some("main"), "schema_version")? {
            // TODO: add some uuid to db schema so we don't mix up migrations?
            tx.execute(
                "CREATE TABLE schema_version (version TEXT NOT NULL) STRICT",
                [],
            )?;
            tx.execute("INSERT INTO schema_version (version) VALUES (?1)", ["v1"])?;
        }

        let mut version: String =
            tx.query_one("SELECT version FROM schema_version", [], |row| row.get(0))?;

        loop {
            if version == "v1" {
                tx.execute("CREATE TABLE state (id TEXT PRIMARY KEY NOT NULL, state TEXT NOT NULL, version INTEGER NOT NULL) STRICT", [])?;
                tx.execute(
                    "INSERT INTO state (id, state, version) VALUES (?1, ?2, ?3)",
                    ("selfupdate", serde_json::to_string(&State::initial())?, 1),
                )?;
                version = "v2".into();
            } else if version == "v2" {
                tx.execute(
                    "CREATE TABLE tracked_resources (
                        kind TEXT NOT NULL,
                        id TEXT NOT NULL,
                        PRIMARY KEY (kind, id)
                    ) STRICT",
                    [],
                )?;
                version = "v3".into();
            } else {
                break;
            }
            tx.execute("UPDATE schema_version SET version = ?1", [&version])?;
        }

        tx.commit()?;

        Ok(Storage { pool: pool })
    }

    pub(crate) fn update_state(&self, new_state: &State, old_version: i64) -> Result<i64> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;

        // fetch the old state so we can print a diff
        let (state_str, version): (String, i64) = tx.query_one(
            "SELECT state, version FROM state WHERE id = ?1",
            ["selfupdate"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let old_state: State = serde_json::from_str(&state_str)?;
        if version != old_version {
            bail!("state version mismatch; state changed by another process?");
        }
        info!("updating state: {}", diff_states(&old_state, new_state)?);
        tx.execute(
            "UPDATE state SET version = ?1, state = ?2 WHERE id = ?3",
            (
                old_version + 1,
                serde_json::to_string(new_state)?,
                "selfupdate",
            ),
        )?;
        tx.commit()?;

        /*
        let changed = self.pool.get()?.execute(
            "UPDATE state SET version = ?1, state = ?2 WHERE id = ?3 and version = ?4",
            (
                old_version + 1,
                serde_json::to_string(new_state)?,
                "selfupdate",
                old_version,
            ),
        )?;
        if changed != 1 {
            bail!("state version mismatch; state changed by another process?");
        }
        info!("updated state to {:?}", new_state);
        */

        Ok(old_version + 1)
    }

    pub(crate) fn get_state(&self) -> Result<(State, i64)> {
        let (state_str, version): (String, i64) = self.pool.get()?.query_one(
            "SELECT state, version FROM state WHERE id = ?1",
            ["selfupdate"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((serde_json::from_str(&state_str)?, version))
    }

    pub(crate) fn add_tracked_resources(&self, kind: &str, id: &str) -> Result<()> {
        self.pool.get()?.execute("INSERT INTO tracked_resources (kind, id) VALUES (?1, ?2) ON CONFLICT (kind, id) DO NOTHING", [kind, id])?;
        Ok(())
    }

    pub(crate) fn remove_tracked_resource(&self, kind: &str, id: &str) -> Result<()> {
        self.pool.get()?.execute(
            "DELETE FROM tracked_resources WHERE kind = ?1 AND id = ?2",
            [kind, id],
        )?;
        Ok(())
    }

    pub(crate) fn list_tracked_resources(&self, kind: &str) -> Result<Vec<String>> {
        let mut ids = vec![];
        for id in self
            .pool
            .get()?
            .prepare("SELECT id FROM tracked_resources WHERE kind = ?1")?
            .query_map([kind], |row| row.get(0))?
        {
            ids.push(id?);
        }
        Ok(ids)
    }
}
