use std::sync::{Arc, Mutex};
use diesel::sqlite::SqliteConnection;
use crate::sql::runtime::connection::ConnectionPool;
use crate::sql::runtime::errors::DbError;

/// An execution context for database operations, either using the pool or an active transaction.
#[derive(Clone)]
pub enum Exec {
    Pool(ConnectionPool),
    Transaction(Arc<Mutex<SqliteConnection>>),
}

impl Exec {
    pub fn new_pool(pool: ConnectionPool) -> Self {
        Self::Pool(pool)
    }

    pub fn new_transaction(conn: Arc<Mutex<SqliteConnection>>) -> Self {
        Self::Transaction(conn)
    }

    /// Run a synchronous database closure inside a blocking task context.
    pub fn run<F, R>(&self, f: F) -> Result<R, DbError>
    where
        F: FnOnce(&mut SqliteConnection) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        match self {
            Self::Pool(pool) => {
                let mut conn = pool.get()?;
                f(&mut conn)
            }
            Self::Transaction(conn_mutex) => {
                let mut conn = conn_mutex.lock().unwrap();
                f(&mut conn)
            }
        }
    }
}
