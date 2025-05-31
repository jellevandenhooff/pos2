use std::sync::Arc;

use generic_array::GenericArray;
use rand::TryRngCore;
use rand::rngs::OsRng;
use rusqlite::OptionalExtension;
use wasmtime::Result;

#[derive(Clone)]
pub struct UserAppModel {
    pub id: i32,
    pub user_id: i32,
    pub name: String,
    pub version: String,
    pub path: String,
}

// UserAppVersion (maybe)? so we can track version(s)
// then... have multiple versions, and a current version?
// or... just a current version and allow switching?
// config: auto update yes/no

pub struct NewUserApp<'a> {
    pub user_id: &'a i32,
    pub name: &'a String,
    pub version: &'a String,
    pub path: &'a String,
}

pub struct UpdateUserApp<'a> {
    pub user_id: &'a i32,
    pub name: &'a String,
    pub version: &'a String,
    pub path: &'a String,
}

pub struct RegistryAppModel {
    pub id: i32,
    pub name: String,
    pub version: String,
    pub oci_reference: String,
}

// 1 : N RegistryApp : RegistryAppVersions
// RegistryAppVersion

pub struct NewRegistryApp<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub oci_reference: &'a str,
}

pub struct UserModel {
    pub id: i32,
    pub name: String,
    // name should become domain; email as recovery
}

pub struct NewUser<'a> {
    pub name: &'a str,
}

pub struct UserSessionModel {
    pub id: String,
    pub user_id: i32,
}

pub struct NewUserSession<'a> {
    pub id: &'a str,
    pub user_id: &'a i32,
}

pub struct LoginTokenModel {
    pub id: String,
    pub domain: String,
    pub user_id: i32,
    pub expiration: chrono::DateTime<chrono::Utc>,
}

pub struct NewLoginToken<'a> {
    pub id: &'a String,
    pub domain: &'a String,
    pub user_id: &'a i32,
    pub expires_at: &'a chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct Storage {
    conn: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
}

impl Storage {
    pub fn new() -> Result<Self> {
        let conn = rusqlite::Connection::open("./data/server.sqlite3")?;

        Ok(Storage {
            conn: Arc::new(tokio::sync::Mutex::new(conn)),
        })
    }

    /*
    pub async fn list_apps(&self) -> Result<HashMap<String, App>> {
        let mut res = HashMap::new();
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached("SELECT name, path FROM apps ORDER BY name")?;
        let mut rows = stmt.query(())?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let path = row.get(1)?;
            res.insert(
                name.clone(),
                App {
                    name: name,
                    path: path,
                },
            );
        }

        Ok(res)
    }

    pub async fn add_app(&self, app: &App) -> Result<()> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached("INSERT INTO apps (name, path) VALUES (?, ?)")?;
        let n = stmt.execute((&app.name, &app.path))?;
        if n != 1 {
            anyhow::bail!("expected 1 changed row, got {}", n);
        }
        Ok(())
    }
    */

    pub async fn list_users(&self) -> Result<Vec<UserModel>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached("SELECT id, name FROM users")?;

        let rows = stmt.query_and_then([], |row| {
            Ok::<_, anyhow::Error>(UserModel {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        })?;

        let mut users = vec![];
        for row in rows {
            users.push(row?);
        }

        return Ok(users);
    }

    pub async fn list_user_apps_by_user_id(&self, user_id: &i32) -> Result<Vec<UserAppModel>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached(
            "SELECT id, user_id, name, version, path FROM user_apps WHERE user_id = ?",
        )?;

        let rows = stmt.query_and_then((user_id,), |row| {
            Ok::<_, anyhow::Error>(UserAppModel {
                id: row.get(0)?,
                user_id: row.get(1)?,
                name: row.get(2)?,
                version: row.get(3)?,
                path: row.get(4)?,
            })
        })?;

        let mut user_apps = vec![];
        for row in rows {
            user_apps.push(row?);
        }

        return Ok(user_apps);
    }

    pub async fn insert_user_app(&self, app: &NewUserApp<'_>) -> Result<i32> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached(
            "INSERT INTO user_apps (user_id, name, version, path) VALUES (?, ?, ?, ?) RETURNING id",
        )?;

        let id: i32 = stmt.query_row((app.user_id, app.name, app.version, app.path), |row| {
            Ok(row.get(0)?)
        })?;

        return Ok(id);
    }

    pub async fn update_user_app(&self, app: &UpdateUserApp<'_>) -> Result<()> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached(
            "UPDATE user_apps SET version = ?, path = ? WHERE user_id = ? AND name = ?",
        )?;

        stmt.execute((app.version, app.path, app.user_id, app.name))?;
        Ok(())
    }

    pub async fn list_registry_apps(&self) -> Result<Vec<RegistryAppModel>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached(
            "SELECT id, name, version, oci_reference FROM registry_apps ORDER BY name, version",
        )?;

        let rows = stmt.query_and_then([], |row| {
            Ok::<_, anyhow::Error>(RegistryAppModel {
                id: row.get(0)?,
                name: row.get(1)?,
                version: row.get(2)?,
                oci_reference: row.get(3)?,
            })
        })?;

        let mut registry_apps = vec![];
        for row in rows {
            registry_apps.push(row?);
        }

        return Ok(registry_apps);
    }

    pub async fn insert_registry_app(&self, app: &NewRegistryApp<'_>) -> Result<i32> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached(
            "INSERT INTO registry_apps (name, version, oci_reference) VALUES (?, ?, ?) RETURNING id",
        )?;

        let id: i32 = stmt.query_row([app.name, app.version, app.oci_reference], |row| {
            Ok(row.get(0)?)
        })?;

        return Ok(id);
    }

    pub async fn get_user_by_id(&self, id: &i32) -> Result<UserModel> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached("SELECT id, name FROM users WHERE id = ?")?;
        let user = stmt.query_row([id], |row| {
            Ok(UserModel {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        })?;

        return Ok(user);
    }

    pub async fn create_session_for_user(&self, user: &UserModel) -> Result<UserSessionModel> {
        let conn = self.conn.lock().await;

        let mut key = [0u8; 16];
        OsRng.try_fill_bytes(&mut key).unwrap();
        let array = GenericArray::from_array(key);
        let session_id = format!("{:x}", array);

        let mut stmt =
            conn.prepare_cached("INSERT INTO user_sessions (id, user_id) VALUES (?, ?)")?;
        stmt.execute((&session_id, &user.id))?;
        Ok(UserSessionModel {
            id: session_id,
            user_id: user.id.clone(),
        })
    }

    pub async fn get_user_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<(UserSessionModel, UserModel)>> {
        let session: UserSessionModel;

        {
            let conn = self.conn.lock().await;

            let mut stmt =
                conn.prepare_cached("SELECT id, user_id FROM user_sessions WHERE id = ?")?;

            session = match stmt
                .query_row([session_id], |row| {
                    Ok(UserSessionModel {
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                    })
                })
                .optional()?
            {
                Some(session) => session,
                None => return Ok(None),
            };
        }

        let user = self.get_user_by_id(&session.user_id).await?;

        Ok(Some((session, user)))
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;

        let mut stmt = conn.prepare_cached("DELETE FROM user_sessions WHERE id = ?")?;

        stmt.execute([session_id])?;

        Ok(())
    }
}
