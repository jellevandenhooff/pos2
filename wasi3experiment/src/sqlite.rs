use wasmtime::component::{Accessor, Linker, Resource, ResourceTable};
use wasmtime::Result;
use wasmtime_wasi::WasiView;

pub struct Transaction {
    // transaction: rusqlite::Transaction<'static>,
    running: bool,
}

pub struct SqliteCtx {
    pub conn: rusqlite::Connection,
    pub in_tx: bool,
}

impl SqliteCtx {
    pub fn new(conn: rusqlite::Connection) -> Self {
        Self {
            conn: conn,
            in_tx: false,
        }
    }
}

impl wasmtime::component::HasData for Sqlite {
    type Data<'a> = SqliteCtxView<'a>;
}

struct Sqlite;

use crate::generated::jelle::test::sqlite;

impl sqlite::Host for SqliteCtxView<'_> {}
impl sqlite::HostTransaction for SqliteCtxView<'_> {}

pub trait SqliteView: Send {
    fn sqlite(&mut self) -> SqliteCtxView<'_>;
}

pub struct SqliteCtxView<'a> {
    pub ctx: &'a mut SqliteCtx,
    pub table: &'a mut ResourceTable,
}

pub fn sqlite_add_to_linker<T>(linker: &mut Linker<T>) -> wasmtime::Result<()>
where
    T: SqliteView + 'static,
{
    sqlite::add_to_linker::<_, Sqlite>(linker, T::sqlite)?;
    Ok(())
}

impl From<rusqlite::Error> for sqlite::ErrorCode {
    fn from(_value: rusqlite::Error) -> Self {
        sqlite::ErrorCode::Bad
    }
}

use rusqlite::types::FromSqlResult;
use rusqlite::types::ToSqlOutput;
use rusqlite::types::ValueRef;

impl rusqlite::ToSql for sqlite::Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match &self {
            &sqlite::Value::StringValue(v) => {
                Ok(ToSqlOutput::Borrowed(ValueRef::Text(v.as_bytes())))
            }
            &sqlite::Value::S64Value(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Integer(*v))),
            &sqlite::Value::F64Value(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Real(*v))),
            &sqlite::Value::NullValue => Ok(ToSqlOutput::Borrowed(ValueRef::Null)),
            &sqlite::Value::BlobValue(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Blob(v))),
        }
    }
}

impl rusqlite::types::FromSql for sqlite::Value {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Text(s) => Ok(sqlite::Value::StringValue(
                str::from_utf8(s).expect("wtf").into(),
            )),
            ValueRef::Integer(v) => Ok(sqlite::Value::S64Value(v)),
            ValueRef::Real(v) => Ok(sqlite::Value::F64Value(v)),
            ValueRef::Blob(v) => Ok(sqlite::Value::BlobValue(v.into())),
            ValueRef::Null => Ok(sqlite::Value::NullValue),
        }
    }
}

impl sqlite::HostTransactionWithStore for Sqlite {
    async fn query<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<std::result::Result<Vec<sqlite::Row>, sqlite::ErrorCode>> {
        // TODO: deduplicate
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let mut prepared = ctx.conn.prepare(&query)?;
            let count = prepared.column_count(); // TODO: this count may be out of date???????
            let mut rows_iter = prepared.query(rusqlite::params_from_iter(args))?;
            let mut rows = vec![];

            loop {
                let row_accessor = match rows_iter.next()? {
                    None => break,
                    Some(row) => row,
                };
                let mut row = vec![];
                for idx in 0..count {
                    row.push(row_accessor.get(idx)?);
                }
                rows.push(row);
            }

            Ok(Ok(rows))
        })
    }

    async fn execute<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<std::result::Result<u64, sqlite::ErrorCode>> {
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let result = ctx.conn.execute(&query, rusqlite::params_from_iter(args))?;
            Ok(Ok(result as u64))
        })
        // todo!("help");
    }

    async fn commit<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
    ) -> wasmtime::Result<std::result::Result<(), sqlite::ErrorCode>> {
        tracing::info!("commit");
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            tx.running = false;
            ctx.in_tx = false;
            ctx.conn.execute("COMMIT", [])?;
            Ok(Ok(()))
        })
    }

    async fn rollback<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
    ) -> wasmtime::Result<std::result::Result<(), sqlite::ErrorCode>> {
        tracing::info!("rollback");
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            tx.running = false;
            ctx.in_tx = false;
            ctx.conn.execute("ROLLBACK", [])?;
            Ok(Ok(()))
        })
    }

    async fn drop<T>(
        accessor: &Accessor<T, Self>,
        self_: wasmtime::component::Resource<Transaction>,
    ) -> wasmtime::Result<()> {
        tracing::info!("dropping");
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let mut tx = view.table.delete(self_)?;
            if tx.running {
                tx.running = false;
                ctx.in_tx = false;
                ctx.conn.execute("ROLLBACK", [])?;
            }
            drop(tx);
            Ok(())
        })
    }
}

impl sqlite::HostWithStore for Sqlite {
    // TODO: the errors in the http package seem nicer? (it's only one layer of error, somehow?)

    async fn begin<T>(
        accessor: &Accessor<T, Self>,
    ) -> wasmtime::Result<Result<Resource<Transaction>, sqlite::ErrorCode>> {
        tracing::info!("begin");

        accessor.with(|mut store| {
            let ctx = store.get().ctx;
            if ctx.in_tx {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            ctx.conn.execute("BEGIN", [])?;
            ctx.in_tx = true;
            let resource = store.get().table.push(Transaction { running: true })?;
            Ok(Ok(Resource::new_own(resource.rep())))
        })
    }

    async fn query<T>(
        accessor: &Accessor<T, Self>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<Result<Vec<sqlite::Row>, sqlite::ErrorCode>> {
        accessor.with(|mut store| {
            let ctx = store.get().ctx;
            if ctx.in_tx {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let mut prepared = ctx.conn.prepare(&query)?;
            let count = prepared.column_count(); // TODO: this count may be out of date???????
            let mut rows_iter = prepared.query(rusqlite::params_from_iter(args))?;
            let mut rows = vec![];

            loop {
                let row_accessor = match rows_iter.next()? {
                    None => break,
                    Some(row) => row,
                };
                let mut row = vec![];
                for idx in 0..count {
                    row.push(row_accessor.get(idx)?);
                }
                rows.push(row);
            }

            Ok(Ok(rows))
        })
    }

    async fn execute<T>(
        accessor: &Accessor<T, Self>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<Result<u64, sqlite::ErrorCode>> {
        accessor.with(|mut store| {
            let ctx = store.get().ctx;
            if ctx.in_tx {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let result = ctx.conn.execute(&query, rusqlite::params_from_iter(args))?;
            Ok(Ok(result as u64))
        })
    }
}
