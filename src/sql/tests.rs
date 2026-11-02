#[cfg(test)]
mod tests {
    use crate::sql::*;

    const SAMPLE_SCHEMA_JSON: &str = r#"{
      "ir_version": 1,
      "naming": {
        "table_strategy": "snake_case",
        "column_strategy": "snake_case",
        "struct_strategy": "PascalCase"
      },
      "enums": {
        "user_role": {
          "values": ["admin", "editor", "viewer"]
        }
      },
      "tables": {
        "users": {
          "columns": {
            "id": {
              "type": "integer",
              "nullable": false,
              "auto_increment": true
            },
            "name": {
              "type": "text",
              "nullable": false,
              "max_length": 100
            },
            "email": {
              "type": "text",
              "nullable": false
            },
            "age": {
              "type": "integer",
              "nullable": true
            },
            "role": {
              "type": { "enum": { "name": "user_role" } },
              "nullable": false,
              "default": { "kind": "literal", "value": "viewer" }
            },
            "active": {
              "type": "boolean",
              "nullable": false,
              "default": { "kind": "literal", "value": true }
            },
            "created_at": {
              "type": "timestamptz",
              "nullable": false,
              "default": { "kind": "now" }
            }
          },
          "primary_key": {
            "columns": ["id"]
          },
          "unique_constraints": [
            {
              "columns": ["email"]
            }
          ],
          "indexes": [
            {
              "name": "users_name_role_idx",
              "columns": ["name", "role"]
            }
          ]
        },
        "posts": {
          "columns": {
            "id": {
              "type": "integer",
              "nullable": false,
              "auto_increment": true
            },
            "title": {
              "type": "text",
              "nullable": false
            },
            "content": {
              "type": "text",
              "nullable": true
            },
            "author_id": {
              "type": "integer",
              "nullable": false
            },
            "published_at": {
              "type": "timestamptz",
              "nullable": true
            }
          },
          "primary_key": {
            "columns": ["id"]
          },
          "foreign_keys": [
            {
              "columns": ["author_id"],
              "target_table": "users",
              "target_columns": ["id"],
              "on_delete": "CASCADE",
              "on_update": "CASCADE"
            }
          ],
          "indexes": [
            {
              "name": "posts_author_idx",
              "columns": ["author_id"]
            }
          ]
        }
      }
    }"#;

    #[test]
    fn test_compile_schema_sqlite() {
        let artifacts = compile_schema(SAMPLE_SCHEMA_JSON.to_string(), Some("sqlite".to_string())).unwrap();

        // Check Rust models
        assert!(artifacts.rust_models.contains("pub struct User {"));
        assert!(artifacts.rust_models.contains("pub struct Post {"));
        assert!(artifacts.rust_models.contains("pub struct NewUser {"));
        assert!(artifacts.rust_models.contains("pub struct NewPost {"));
        assert!(artifacts.rust_models.contains("pub enum UserRole {"));

        // Check Diesel schema
        assert!(artifacts.diesel_schema.contains("diesel::table! {"));
        assert!(artifacts.diesel_schema.contains("users (id) {"));
        assert!(artifacts.diesel_schema.contains("posts (id) {"));
        assert!(artifacts.diesel_schema.contains("diesel::joinable!(posts -> users (author_id));"));

        // Check SQL DDL
        assert!(artifacts.sql_ddl.contains("PRAGMA foreign_keys = ON;"));
        assert!(artifacts.sql_ddl.contains("CREATE TABLE users ("));
        assert!(artifacts.sql_ddl.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"));
        assert!(artifacts.sql_ddl.contains("CHECK (role IN ('admin', 'editor', 'viewer'))"));
        assert!(artifacts.sql_ddl.contains("DEFAULT CURRENT_TIMESTAMP"));
        assert!(artifacts.sql_ddl.contains("CREATE INDEX users_name_role_idx ON users (name, role);"));

        // Check NAPI bindings
        assert!(artifacts.napi_bindings.contains("#[napi(object)]"));
        assert!(artifacts.napi_bindings.contains("pub struct User {"));

        // Check Validators
        assert!(artifacts.validators.contains("impl User {"));
        assert!(artifacts.validators.contains("self.name.len() > 100"));
    }

    #[test]
    fn test_compile_schema_postgres() {
        let artifacts = compile_schema(SAMPLE_SCHEMA_JSON.to_string(), Some("postgres".to_string())).unwrap();

        // Check SQL DDL
        assert!(artifacts.sql_ddl.contains("CREATE TYPE user_role AS ENUM ('admin', 'editor', 'viewer');"));
        assert!(artifacts.sql_ddl.contains("id SERIAL NOT NULL"));
        assert!(artifacts.sql_ddl.contains("created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()"));
    }

    #[test]
    fn test_validate_schema_json() {
        assert!(validate_schema_json(SAMPLE_SCHEMA_JSON.to_string(), Some("sqlite".to_string())).is_ok());

        // Test with invalid schema JSON
        let invalid_json = SAMPLE_SCHEMA_JSON.replace("\"nullable\": false", "\"nullable\": true");
        let res = validate_schema_json(invalid_json, Some("sqlite".to_string()));
        assert!(res.is_err());
    }

    #[test]
    fn test_generate_migration() {
        let old_schema_json = r#"{
          "ir_version": 1,
          "tables": {
            "users": {
              "columns": {
                "id": {
                  "type": "integer",
                  "nullable": false,
                  "auto_increment": true
                }
              },
              "primary_key": {
                "columns": ["id"]
              }
            }
          }
        }"#;

        let migration = generate_migration(
            old_schema_json.to_string(),
            SAMPLE_SCHEMA_JSON.to_string(),
            Some("sqlite".to_string()),
        )
        .unwrap();

        assert!(migration.changes_count > 0);
        assert!(migration.up_sql.contains("CREATE TABLE posts"));
    }
}
