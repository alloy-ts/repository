//! SQL schema compilation, validation, code generation, and migration diffing engine.
//!
//! Transforms JavaScript DSL schema IR into validated Rust models, Diesel schema definitions,
//! NAPI-RS bindings, SQL DDL, and migration scripts for SQLite and PostgreSQL.

pub mod backend;
pub mod emitters;
pub mod ir;
pub mod migration;
pub mod naming;
pub mod validation;

pub use backend::{Backend, PostgresBackend, SqliteBackend};
pub use emitters::{
    emit_ddl, emit_diesel_schema, emit_models, emit_napi_bindings, emit_validators,
    ModelEmitterOptions,
};
pub use ir::{
    CheckConstraintIr, ColumnIr, ColumnType, DefaultValueIr, EnumIr, ForeignKeyIr, IndexIr,
    NamingConfig, PrimaryKeyIr, ReferentialAction, SchemaIr, TableIr, CURRENT_IR_VERSION,
};
pub use migration::{diff_schemas, emit_migration, MigrationError, MigrationSql, SchemaChange};
pub use validation::{validate_schema, ValidationError};

pub mod dsl;
pub mod ir_test;
pub mod query;

pub use dsl::*;
pub use query::*;

#[cfg(test)]
mod tests;

use napi_derive::napi;

/// Generated code collection across all emitters.
#[napi(object)]
pub struct GeneratedArtifacts {
    /// Rust Row and NewX insert models.
    pub rust_models: String,
    /// Diesel schema table! and joinable! definitions.
    pub diesel_schema: String,
    /// SQL DDL CREATE TABLE and index statements.
    pub sql_ddl: String,
    /// NAPI-RS object binding structs.
    pub napi_bindings: String,
    /// Runtime validator validate() method implementations.
    pub validators: String,
}

/// Compile a raw JSON Schema IR string, validate against the selected backend ("sqlite" or "postgres"),
/// and return generated code artifacts.
#[napi]
pub fn compile_schema(schema_json: String, backend_name: Option<String>) -> napi::Result<GeneratedArtifacts> {
    let schema: SchemaIr = serde_json::from_str(&schema_json)
        .map_err(|err| napi::Error::from_reason(format!("failed to parse Schema IR JSON: {err}")))?;

    let is_postgres = backend_name
        .as_deref()
        .map(|b| b.eq_ignore_ascii_case("postgres") || b.eq_ignore_ascii_case("postgresql"))
        .unwrap_or(false);

    if is_postgres {
        let backend = PostgresBackend;
        validate_schema(&schema, &backend)
            .map_err(|errs| napi::Error::from_reason(format_validation_errors(&errs)))?;

        let rust_models = emit_models(
            &schema,
            &backend,
            &ModelEmitterOptions {
                serde: true,
                diesel: true,
                napi: true,
            },
        );
        let diesel_schema = emit_diesel_schema(&schema, &backend);
        let sql_ddl = emit_ddl(&schema, &backend);
        let napi_bindings = emit_napi_bindings(&schema, &backend);
        let validators = emit_validators(&schema);

        Ok(GeneratedArtifacts {
            rust_models,
            diesel_schema,
            sql_ddl,
            napi_bindings,
            validators,
        })
    } else {
        let backend = SqliteBackend;
        validate_schema(&schema, &backend)
            .map_err(|errs| napi::Error::from_reason(format_validation_errors(&errs)))?;

        let rust_models = emit_models(
            &schema,
            &backend,
            &ModelEmitterOptions {
                serde: true,
                diesel: true,
                napi: true,
            },
        );
        let diesel_schema = emit_diesel_schema(&schema, &backend);
        let sql_ddl = emit_ddl(&schema, &backend);
        let napi_bindings = emit_napi_bindings(&schema, &backend);
        let validators = emit_validators(&schema);

        Ok(GeneratedArtifacts {
            rust_models,
            diesel_schema,
            sql_ddl,
            napi_bindings,
            validators,
        })
    }
}

/// Validate a Schema IR JSON string against logical integrity rules and target backend.
#[napi]
pub fn validate_schema_json(schema_json: String, backend_name: Option<String>) -> napi::Result<bool> {
    let schema: SchemaIr = serde_json::from_str(&schema_json)
        .map_err(|err| napi::Error::from_reason(format!("failed to parse Schema IR JSON: {err}")))?;

    let is_postgres = backend_name
        .as_deref()
        .map(|b| b.eq_ignore_ascii_case("postgres") || b.eq_ignore_ascii_case("postgresql"))
        .unwrap_or(false);

    let res = if is_postgres {
        validate_schema(&schema, &PostgresBackend)
    } else {
        validate_schema(&schema, &SqliteBackend)
    };

    match res {
        Ok(()) => Ok(true),
        Err(errs) => Err(napi::Error::from_reason(format_validation_errors(&errs))),
    }
}

/// Diff an old Schema IR snapshot JSON with a new Schema IR JSON and generate up and down migration SQL.
#[napi(object)]
pub struct GeneratedMigration {
    pub up_sql: String,
    pub down_sql: String,
    pub changes_count: u32,
}

#[napi]
pub fn generate_migration(
    old_schema_json: String,
    new_schema_json: String,
    backend_name: Option<String>,
) -> napi::Result<GeneratedMigration> {
    let old_schema: SchemaIr = serde_json::from_str(&old_schema_json)
        .map_err(|err| napi::Error::from_reason(format!("failed to parse old Schema IR JSON: {err}")))?;
    let new_schema: SchemaIr = serde_json::from_str(&new_schema_json)
        .map_err(|err| napi::Error::from_reason(format!("failed to parse new Schema IR JSON: {err}")))?;

    let is_postgres = backend_name
        .as_deref()
        .map(|b| b.eq_ignore_ascii_case("postgres") || b.eq_ignore_ascii_case("postgresql"))
        .unwrap_or(false);

    let changes = diff_schemas(&old_schema, &new_schema)
        .map_err(|err| napi::Error::from_reason(format!("schema diff failed: {err}")))?;

    let migration = if is_postgres {
        emit_migration(&changes, &new_schema, &PostgresBackend)
            .map_err(|err| napi::Error::from_reason(format!("migration emit failed: {err}")))?
    } else {
        emit_migration(&changes, &new_schema, &SqliteBackend)
            .map_err(|err| napi::Error::from_reason(format!("migration emit failed: {err}")))?
    };

    Ok(GeneratedMigration {
        up_sql: migration.up_sql,
        down_sql: migration.down_sql,
        changes_count: changes.len() as u32,
    })
}

fn format_validation_errors(errs: &[ValidationError]) -> String {
    let mut msg = format!("Validation failed with {} error(s):\n", errs.len());
    for (i, err) in errs.iter().enumerate() {
        msg.push_str(&format!("  {}. {}\n", i + 1, err));
    }
    msg
}
