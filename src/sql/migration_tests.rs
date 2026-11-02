#[cfg(test)]
mod tests {
    use diesel::connection::SimpleConnection;
    use crate::sql::backend::{Backend, PostgresBackend, SqliteBackend};
    use crate::sql::ir::*;
    use crate::sql::migration::*;
    use crate::sql::runtime::{ConnectionOptions, ConnectionPool};
    use std::collections::BTreeMap;

    fn build_initial_schema() -> SchemaIr {
        let mut tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();

        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        let name_col = ColumnIr::new(ColumnType::Text);
        users_cols.insert("name".to_string(), name_col);

        tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: Vec::new(),
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables,
        }
    }

    fn build_updated_schema() -> SchemaIr {
        let mut initial = build_initial_schema();

        // Add email column
        let mut email_col = ColumnIr::new(ColumnType::Text);
        email_col.nullable = true;
        initial
            .tables
            .get_mut("users")
            .unwrap()
            .columns
            .insert("email".to_string(), email_col);

        // Add posts table
        let mut posts_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        posts_cols.insert("id".to_string(), id_col);

        let title_col = ColumnIr::new(ColumnType::Text);
        posts_cols.insert("title".to_string(), title_col);

        let author_col = ColumnIr::new(ColumnType::Integer);
        posts_cols.insert("author_id".to_string(), author_col);

        initial.tables.insert(
            "posts".to_string(),
            TableIr {
                columns: posts_cols,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: vec![ForeignKeyIr {
                    columns: vec!["author_id".to_string()],
                    target_table: "users".to_string(),
                    target_columns: vec!["id".to_string()],
                    on_delete: ReferentialAction::Cascade,
                    on_update: ReferentialAction::Cascade,
                    name: None,
                }],
                unique_constraints: Vec::new(),
                indexes: vec![IndexIr {
                    name: "posts_author_idx".to_string(),
                    columns: vec!["author_id".to_string()],
                    unique: false,
                }],
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        initial
    }

    #[test]
    fn test_phase4_sqlite_migration_execution_against_database() {
        let old_schema = build_initial_schema();
        let new_schema = build_updated_schema();

        let pool = ConnectionPool::new(":memory:", ConnectionOptions::default()).unwrap();
        let mut conn = pool.get().unwrap();

        // 1. Apply initial schema CREATE TABLE
        let initial_ddl = crate::sql::emitters::emit_ddl(&old_schema, &SqliteBackend);
        conn.batch_execute(&initial_ddl).unwrap();

        // Seed some data in users
        conn.batch_execute("INSERT INTO users (id, name) VALUES (1, 'Alice');").unwrap();

        // 2. Diff schemas and generate migration
        let diff = diff_schemas(&old_schema, &new_schema).unwrap();
        assert_eq!(diff.len(), 2); // add column email to users, create table posts (which includes index)

        let migration = emit_migration(&diff, &new_schema, &SqliteBackend).unwrap();

        // 3. Apply up.sql migration
        conn.batch_execute(&migration.up_sql).unwrap();

        // Verify seeded data preserved and new schema active
        conn.batch_execute("INSERT INTO posts (id, title, author_id) VALUES (1, 'Hello World', 1);").unwrap();
        let check_users = conn.batch_execute("SELECT id, name, email FROM users;");
        assert!(check_users.is_ok());
    }

    #[test]
    fn test_phase4_postgres_migration_up_and_down_generation() {
        let old_schema = build_initial_schema();
        let new_schema = build_updated_schema();

        let diff = diff_schemas(&old_schema, &new_schema).unwrap();
        let migration = emit_migration(&diff, &new_schema, &PostgresBackend).unwrap();

        assert!(migration.up_sql.contains("ALTER TABLE users ADD COLUMN email TEXT;"));
        assert!(migration.up_sql.contains("CREATE TABLE posts"));
        assert!(migration.down_sql.contains("DROP TABLE posts;"));
        assert!(migration.down_sql.contains("ALTER TABLE users DROP COLUMN email;"));
    }
}
