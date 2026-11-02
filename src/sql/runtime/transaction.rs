use std::sync::{Arc, Mutex};
use diesel::connection::SimpleConnection;
use diesel::sqlite::SqliteConnection;
use crate::sql::runtime::errors::{DbError, DbErrorCode};
use crate::sql::runtime::exec::Exec;

/// Active database transaction handle.
pub struct TransactionHandle {
    conn: Arc<Mutex<SqliteConnection>>,
    committed: bool,
}

impl TransactionHandle {
    /// Begin an immediate transaction on a dedicated SQLite connection.
    pub fn begin(mut conn: SqliteConnection) -> Result<Self, DbError> {
        conn.batch_execute("BEGIN IMMEDIATE TRANSACTION;")
            .map_err(|e| DbError::new(DbErrorCode::Internal, format!("Failed to begin transaction: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            committed: false,
        })
    }

    /// Obtain an Exec context bound to this transaction.
    pub fn exec(&self) -> Exec {
        Exec::new_transaction(Arc::clone(&self.conn))
    }

    /// Commit the active transaction.
    pub fn commit(mut self) -> Result<(), DbError> {
        let mut conn = self.conn.lock().unwrap();
        conn.batch_execute("COMMIT;")
            .map_err(|e| DbError::new(DbErrorCode::Internal, format!("Failed to commit transaction: {e}")))?;
        self.committed = true;
        Ok(())
    }

    /// Roll back the active transaction.
    pub fn rollback(mut self) -> Result<(), DbError> {
        let mut conn = self.conn.lock().unwrap();
        conn.batch_execute("ROLLBACK;")
            .map_err(|e| DbError::new(DbErrorCode::Internal, format!("Failed to rollback transaction: {e}")))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TransactionHandle {
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(mut conn) = self.conn.lock() {
                let _ = conn.batch_execute("ROLLBACK;");
            }
        }
    }
}
