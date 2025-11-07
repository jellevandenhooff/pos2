use crate::entity::{
    auth_tokens, device_tokens, dns_records, prelude::*, sessions, user_domains, users,
};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, Transaction};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    IntoActiveModel, QueryFilter, Select, TransactionTrait, sea_query::OnConflict,
};

#[derive(Clone)]
pub struct DB {
    conn: DatabaseConnection,
    path: String,
}

#[derive(Clone)]
pub struct User {
    pub id: String, // UUID?
    pub email: String,
    pub github_user_id: Option<i64>,
    pub domains: Vec<String>,
}

impl DB {
    pub async fn new(env: &crate::common::Environment) -> Result<Self> {
        let path = env.join_path("db.sqlite3");

        Self::migrate(&path)?;
        let conn = Database::connect(format!("sqlite://{}", path)).await?;
        let db = Self { conn, path };
        Ok(db)
    }

    pub async fn create_user(
        &self,
        email: &str,
        github_user_id: Option<i64>,
        domains: Vec<String>,
    ) -> Result<User> {
        let id = crate::web::make_random_hex_string();

        let tx = self.conn.begin().await?;

        Users::insert(users::ActiveModel {
            id: Set(id.clone()),
            email: Set(email.into()),
            github_user_id: Set(github_user_id),
        })
        .exec(&tx)
        .await?;

        for domain in domains.iter() {
            UserDomains::insert(
                user_domains::Model {
                    user_id: id.clone(),
                    domain: domain.clone(),
                }
                .into_active_model(),
            )
            .exec(&tx)
            .await?;
        }

        tx.commit().await?;

        let user = User {
            id: id,
            email: email.into(),
            github_user_id: github_user_id,
            domains: domains,
        };

        Ok(user)
    }

    async fn get_user_internal(
        tx: &impl ConnectionTrait,
        query: Select<users::Entity>,
    ) -> Result<Option<User>> {
        let user = match query.one(tx).await? {
            Some(user) => user,
            None => return Ok(None),
        };

        let domains = UserDomains::find()
            .filter(user_domains::Column::UserId.eq(&user.id))
            .all(tx)
            .await?
            .into_iter()
            .map(|model| model.domain)
            .collect();

        Ok(Some(User {
            id: user.id,
            email: user.email,
            github_user_id: user.github_user_id,
            domains: domains,
        }))
    }

    pub async fn get_user_by_id(&self, user_id: &str) -> Result<Option<User>> {
        let tx = self.conn.begin().await?;
        let result = Self::get_user_internal(&tx, Users::find_by_id(user_id)).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn get_user_by_github_user_id(&self, github_user_id: i64) -> Result<Option<User>> {
        let tx = self.conn.begin().await?;
        let result = Self::get_user_internal(
            &tx,
            Users::find().filter(users::Column::GithubUserId.eq(Some(github_user_id))),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let tx = self.conn.begin().await?;
        let result = Self::get_user_internal(
            &tx,
            Users::find().filter(users::Column::Email.eq(email)),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn delete_session_token(&self, session_id: &str) -> Result<()> {
        Sessions::delete_by_id(session_id).exec(&self.conn).await?;
        Ok(())
    }

    pub async fn create_session_token(&self, user_id: &str) -> Result<sessions::Model> {
        let session = sessions::ActiveModel {
            id: Set(crate::web::make_random_hex_string()),
            user_id: Set(user_id.into()),
        };
        Ok(Sessions::insert(session)
            .exec_with_returning(&self.conn)
            .await?)
    }

    pub async fn get_session_token_by_id(
        &self,
        session_id: &str,
    ) -> Result<Option<sessions::Model>> {
        Ok(Sessions::find_by_id(session_id).one(&self.conn).await?)
    }

    pub async fn create_auth_token(&self, user_id: &str) -> Result<auth_tokens::Model> {
        let auth_token = auth_tokens::ActiveModel {
            id: Set(crate::web::make_random_hex_string()),
            user_id: Set(user_id.into()),
        };
        Ok(AuthTokens::insert(auth_token)
            .exec_with_returning(&self.conn)
            .await?)
    }

    pub async fn register_user_domain(&self, user_id: &str, domain: &str) -> Result<()> {
        let user_domain = user_domains::ActiveModel {
            user_id: Set(user_id.into()),
            domain: Set(domain.into()),
        };
        UserDomains::insert(user_domain).exec(&self.conn).await?;
        Ok(())
    }

    pub async fn get_device_token_by_device_code(
        &self,
        code: &str,
    ) -> Result<Option<device_tokens::Model>> {
        let token = DeviceTokens::find()
            .filter(device_tokens::Column::DeviceCode.eq(code))
            .one(&self.conn)
            .await?;
        Ok(token)
    }

    pub async fn get_device_token_by_user_code(
        &self,
        code: &str,
    ) -> Result<Option<device_tokens::Model>> {
        let token = DeviceTokens::find()
            .filter(device_tokens::Column::UserCode.eq(code))
            .one(&self.conn)
            .await?;
        Ok(token)
    }

    pub async fn insert_device_token(&self, token: &device_tokens::Model) -> Result<()> {
        DeviceTokens::insert(token.clone().into_active_model())
            .exec(&self.conn)
            .await?;
        Ok(())
    }

    pub async fn update_device_token(&self, token: &device_tokens::Model) -> Result<()> {
        DeviceTokens::update(device_tokens::ActiveModel {
            device_code: Set(token.device_code.clone()),
            state: Set(token.state.clone()),
            approved_by_user_id: Set(token.approved_by_user_id.clone()),
            next_poll_at: Set(token.next_poll_at.clone()),
            expires_at: Set(token.expires_at.clone()),
            interval_seconds: Set(token.interval_seconds),
            ..Default::default()
        })
        .exec(&self.conn)
        .await?;
        Ok(())
    }

    pub async fn get_auth_token_by_id(&self, token_id: &str) -> Result<Option<auth_tokens::Model>> {
        let token = AuthTokens::find_by_id(token_id).one(&self.conn).await?;
        Ok(token)
    }

    pub async fn write_dns_record(
        &self,
        name: &str,
        kind: &str,
        value: &str,
        ttl: i32,
        expires_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<()> {
        DnsRecords::insert(dns_records::ActiveModel {
            name: Set(name.into()),
            kind: Set(kind.into()),
            value: Set(value.into()),
            ttl_seconds: Set(ttl),
            expires_at: Set(expires_at),
        })
        .on_conflict(
            OnConflict::columns([dns_records::Column::Name, dns_records::Column::Kind])
                .update_columns([
                    dns_records::Column::Value,
                    dns_records::Column::TtlSeconds,
                    dns_records::Column::ExpiresAt,
                ])
                .to_owned(),
        )
        .exec(&self.conn)
        .await?;
        Ok(())
    }

    pub async fn get_records(&self) -> Result<Vec<dns_records::Model>> {
        let records = DnsRecords::find().all(&self.conn).await?;
        Ok(records)
    }

    fn create_version_table(tx: &Transaction, version: String) -> Result<String> {
        tx.execute(
            "CREATE TABLE schema_version (current_version TEXT PRIMARY KEY NOT NULL)",
            [],
        )?;
        tx.execute(
            "INSERT INTO schema_version (current_version) VALUES (?)",
            [&version],
        )?;

        Ok(version)
    }

    fn get_version(tx: &Transaction) -> Result<String> {
        let version: String =
            tx.query_row_and_then("SELECT current_version FROM schema_version", [], |r| {
                r.get(0)
            })?;

        Ok(version)
    }

    fn update_version(tx: &Transaction, version: String) -> Result<String> {
        tx.execute("UPDATE schema_version SET current_version = ?", [&version])?;
        Ok(version)
    }

    fn migrate(path: &str) -> Result<()> {
        let mut conn = Connection::open(path)?;

        let tx = conn.transaction()?;

        let mut current_version: String = match Self::get_version(&tx) {
            Ok(v) => v,
            Err(_) => Self::create_version_table(&tx, "v0".into())?,
        };

        if current_version == "v0" {
            tx.execute(
                "
            CREATE TABLE users (
                id TEXT PRIMARY KEY NOT NULL,
                email TEXT NOT NULL,
                github_user_id INTEGER
            )",
                [],
            )?;

            tx.execute(
                "
            CREATE TABLE user_domains (
                user_id TEXT NOT NULL,
                domain TEXT NOT NULL,
                PRIMARY KEY (user_id, domain)
            )",
                [],
            )?;

            tx.execute(
                "
            CREATE UNIQUE INDEX idx_users_email ON users (email)
            ",
                [],
            )?;

            tx.execute(
                "
            CREATE UNIQUE INDEX idx_users_github_user_id ON users (github_user_id) WHERE github_user_id IS NOT NULL
            ",
                [],
            )?;

            tx.execute(
                "
            CREATE UNIQUE INDEX idx_user_domains_domain ON user_domains (domain)
            ",
                [],
            )?;

            current_version = Self::update_version(&tx, "v1".into())?;
        }

        if current_version == "v1" {
            tx.execute(
                "
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY NOT NULL,
                user_id TEXT NOT NULL
            )",
                [],
            )?;

            current_version = Self::update_version(&tx, "v2".into())?;
        }

        if current_version == "v2" {
            tx.execute(
                "
            CREATE TABLE auth_tokens (
                id TEXT PRIMARY KEY NOT NULL,
                user_id TEXT NOT NULL
            )",
                [],
            )?;

            current_version = Self::update_version(&tx, "v3".into())?;
        }

        if current_version == "v3" {
            tx.execute(
                "
            CREATE TABLE device_tokens (
                device_code TEXT PRIMARY KEY NOT NULL,
                user_code TEXT NOT NULL,
                state TEXT NOT NULL,
                approved_by_user_id TEXT NOT NULL,
                next_poll_at TEXT NOT NULL,
                interval_seconds INTEGER NOT NULL,
                expires_at TEXT NOT NULL
            )",
                [],
            )?;

            tx.execute(
                "
            CREATE UNIQUE INDEX idx_device_tokens_user_code ON device_tokens (user_code)
            ",
                [],
            )?;

            current_version = Self::update_version(&tx, "v4".into())?;
        }

        if current_version == "v4" {
            // TODO: zone?
            tx.execute(
                "
            CREATE TABLE dns_records (
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                value TEXT NOT NULL,
                ttl_seconds INTEGER NOT NULL,
                expires_at TEXT,
                PRIMARY KEY (name, kind)
            )",
                [],
            )?;

            current_version = Self::update_version(&tx, "v5".into())?;
        }
        _ = current_version;

        tx.commit()?;
        Ok(())
    }
}
