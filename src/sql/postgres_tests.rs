#[cfg(test)]
mod tests {
    use crate::sql::backend::{Backend, PostgresBackend, SqliteBackend};
    use crate::sql::emitters::{emit_ddl, emit_postgres_diesel_schema, emit_postgres_sql_types};
    use crate::sql::ir::*;
    use std::collections::BTreeMap;

    fn build_phase3_sample_schema() -> SchemaIr {
        let mut tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();

        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        let mut role_col = ColumnIr::new(ColumnType::Enum {
            name: "user_role".to_string(),
        });
        role_col.default = Some(DefaultValueIr::Literal(serde_json::Value::String(
            "admin".to_string(),
        )));
        users_cols.insert("role".to_string(), role_col);

        let mut created_at = ColumnIr::new(ColumnType::Timestamptz);
        created_at.default = Some(DefaultValueIr::Now);
        users_cols.insert("created_at".to_string(), created_at);

        let uuid_col = ColumnIr::new(ColumnType::Uuid);
        users_cols.insert("uuid".to_string(), uuid_col);

        let json_col = ColumnIr::new(ColumnType::Json);
        users_cols.insert("meta".to_string(), json_col);

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
                indexes: vec![IndexIr {
                    name: "users_role_idx".to_string(),
                    columns: vec!["role".to_string()],
                    unique: false,
                }],
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        let mut enums = BTreeMap::new();
        enums.insert(
            "user_role".to_string(),
            EnumIr {
                values: vec!["admin".to_string(), "editor".to_string(), "viewer".to_string()],
                rust_name: None,
            },
        );

        SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums,
            tables,
        }
    }

    #[test]
    fn test_postgres_type_mapping_and_ddl() {
        let schema = build_phase3_sample_schema();
        let pg = PostgresBackend;

        let ddl = emit_ddl(&schema, &pg);
        assert!(ddl.contains("CREATE TYPE user_role AS ENUM ('admin', 'editor', 'viewer');"));
        assert!(ddl.contains("id SERIAL NOT NULL"));
        assert!(ddl.contains("role user_role NOT NULL DEFAULT 'admin'"));
        assert!(ddl.contains("created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()"));
        assert!(ddl.contains("uuid UUID NOT NULL"));
        assert!(ddl.contains("meta JSONB NOT NULL"));
    }

    #[test]
    fn test_sqlite_type_mapping_and_ddl() {
        let schema = build_phase3_sample_schema();
        let sqlite = SqliteBackend;

        let ddl = emit_ddl(&schema, &sqlite);
        assert!(ddl.contains("PRAGMA foreign_keys = ON;"));
        assert!(ddl.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"));
        assert!(ddl.contains("CHECK (role IN ('admin', 'editor', 'viewer'))"));
        assert!(ddl.contains("created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"));
        assert!(ddl.contains("uuid TEXT NOT NULL"));
        assert!(ddl.contains("meta TEXT NOT NULL"));
    }

    #[test]
    fn test_postgres_diesel_and_sql_types_emission() {
        let schema = build_phase3_sample_schema();
        let types = emit_postgres_sql_types(&schema);
        assert!(types.contains("pub mod sql_types {"));
        assert!(types.contains("pub struct UserRole;"));

        let schema_code = emit_postgres_diesel_schema(&schema);
        assert!(schema_code.contains("pub mod sql_types"));
        assert!(schema_code.contains("users (id) {"));
        assert!(schema_code.contains("meta -> diesel::sql_types::Jsonb"));
        assert!(schema_code.contains("uuid -> diesel::sql_types::Uuid"));
        assert!(schema_code.contains("created_at -> diesel::sql_types::Timestamptz"));
    }
}
