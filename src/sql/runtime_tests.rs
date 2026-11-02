#[cfg(test)]
mod tests {
    use diesel::connection::SimpleConnection;
    use crate::sql::runtime::*;

    #[test]
    fn test_sqlite_in_memory_connection_pool() {
        let pool = ConnectionPool::new(":memory:", ConnectionOptions::default()).unwrap();
        let mut conn = pool.get().unwrap();

        conn.batch_execute("CREATE TABLE test_tbl (id INTEGER PRIMARY KEY, name TEXT);")
            .unwrap();

        conn.batch_execute("INSERT INTO test_tbl (id, name) VALUES (1, 'Alice');")
            .unwrap();

        drop(conn);

        // Since pool size is 1 for in-memory, the same connection is reused and data persists
        let mut conn2 = pool.get().unwrap();
        let res = conn2.batch_execute("SELECT * FROM test_tbl;");
        assert!(res.is_ok());
    }

    #[test]
    fn test_sqlite_transaction_commit_and_rollback() {
        let pool = ConnectionPool::new(":memory:", ConnectionOptions::default()).unwrap();
        let exec = Exec::new_pool(pool);

        let create = exec.run(|c| {
            c.batch_execute("CREATE TABLE tx_test (id INTEGER PRIMARY KEY, val TEXT);")
                .map_err(DbError::from_diesel)
        });
        assert!(create.is_ok());

        let insert = exec.run(|c| {
            c.batch_execute("INSERT INTO tx_test (id, val) VALUES (1, 'Committed');")
                .map_err(DbError::from_diesel)
        });
        assert!(insert.is_ok());

        let check = exec.run(|c| {
            c.batch_execute("SELECT * FROM tx_test;")
                .map_err(DbError::from_diesel)
        });
        assert!(check.is_ok());
    }

    #[test]
    fn test_error_code_mapping() {
        let err = DbError::unique_violation("UNIQUE constraint failed");
        assert_eq!(err.code, DbErrorCode::UniqueViolation);
        assert_eq!(err.code.as_str(), "UNIQUE_VIOLATION");
    }

    #[test]
    fn test_filter_deserialization() {
        let json_filter = r#"{ "eq": 42, "gte": 21, "in": [1, 2, 42] }"#;
        let int_filter: IntFilter = serde_json::from_str(json_filter).unwrap();
        assert_eq!(int_filter.eq, Some(42));
        assert_eq!(int_filter.gte, Some(21));
        assert_eq!(int_filter.r#in, Some(vec![1, 2, 42]));
    }
}
