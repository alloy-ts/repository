use std::sync::{Arc, Mutex};
use diesel::connection::SimpleConnection;
use diesel::sqlite::SqliteConnection;
use diesel::Connection;
use crate::sql::runtime::errors::{DbError, DbErrorCode};

/// Configuration options for SQLite connection pool.
#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    /// Maximum connections in the pool. For in-memory SQLite, defaults to 1.
    pub max_connections: u32,
    /// Busy timeout in milliseconds.
    pub busy_timeout_ms: u64,
    /// Enable Write-Ahead Logging (WAL) for disk-backed databases.
    pub wal: bool,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            max_connections: 4,
            busy_timeout_ms: 5000,
            wal: true,
        }
    }
}

/// Lightweight connection pool for SQLite connections.
#[derive(Clone)]
pub struct ConnectionPool {
    url: String,
    options: ConnectionOptions,
    connections: Arc<Mutex<Vec<SqliteConnection>>>,
}

impl ConnectionPool {
    /// Establish a connection pool to a SQLite database url (e.g. `file:app.db` or `:memory:`).
    pub fn new(url: &str, mut options: ConnectionOptions) -> Result<Self, DbError> {
        let is_memory = url == ":memory:" || url.contains("mode=memory");
        if is_memory {
            options.max_connections = 1; // In-memory database must share a single connection to preserve state
        }

        let pool = Self {
            url: url.to_string(),
            options,
            connections: Arc::new(Mutex::new(Vec::new())),
        };

        // Create initial connection to ensure database is accessible and configure pragmas
        let conn = pool.create_connection()?;
        pool.connections.lock().unwrap().push(conn);

        Ok(pool)
    }

    /// Obtain a connection from the pool or create a new one.
    pub fn get(&self) -> Result<PooledConnection, DbError> {
        let mut conns = self.connections.lock().unwrap();
        if let Some(conn) = conns.pop() {
            Ok(PooledConnection {
                conn: Some(conn),
                pool: self.clone(),
            })
        } else {
            drop(conns);
            let conn = self.create_connection()?;
            Ok(PooledConnection {
                conn: Some(conn),
                pool: self.clone(),
            })
        }
    }

    fn create_connection(&self) -> Result<SqliteConnection, DbError> {
        let mut conn = SqliteConnection::establish(&self.url)
            .map_err(|e| DbError::new(DbErrorCode::Internal, format!("Failed to connect to SQLite: {e}")))?;

        // Apply mandatory connection settings
        conn.batch_execute("PRAGMA foreign_keys = ON;")
            .map_err(|e| DbError::new(DbErrorCode::Internal, format!("PRAGMA foreign_keys failed: {e}")))?;

        let busy_ms = self.options.busy_timeout_ms;
        conn.batch_execute(&format!("PRAGMA busy_timeout = {busy_ms};"))
            .map_err(|e| DbError::new(DbErrorCode::Internal, format!("PRAGMA busy_timeout failed: {e}")))?;

        if self.options.wal && !self.url.contains(":memory:") {
            let _ = conn.batch_execute("PRAGMA journal_mode = WAL;");
        }

        Ok(conn)
    }

    fn return_connection(&self, conn: SqliteConnection) {
        let mut conns = self.connections.lock().unwrap();
        if (conns.len() as u32) < self.options.max_connections {
            conns.push(conn);
        }
    }
}

/// RAII wrapper for returning SQLite connections to the pool upon drop.
pub struct PooledConnection {
    conn: Option<SqliteConnection>,
    pool: ConnectionPool,
}

impl std::ops::Deref for PooledConnection {
    type Target = SqliteConnection;

    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().unwrap()
    }
}

impl std::ops::DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn.as_mut().unwrap()
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.return_connection(conn);
        }
    }
}
