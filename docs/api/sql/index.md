# SQL Schema Compiler API Documentation

The `repository::sql` module provides a compile-time schema compiler and validation pipeline that turns a schema definition into validated Rust models, Diesel table macros, NAPI-RS bindings, SQL DDL, and migrations for SQLite and PostgreSQL.

## Architecture

The compiler is organized into 4 stages and 5 generators centered around an Intermediate Representation (IR):

```
JavaScript DSL / Schema JSON
           │
           ▼
Schema Intermediate Representation (IR)
           │
           ▼
Rust Validation Pass (`validate_schema`)
           │
           ├─────────────────┬─────────────────┬─────────────────┬─────────────────┐
           ▼                 ▼                 ▼                 ▼                 ▼
      Rust Models      Diesel Schema        SQL DDL          NAPI-RS          Validators
   (Row & NewX struct) (`table!`, `joinable!`) (CREATE TABLE)  (#[napi(object)]) (`validate()`)
```

- **JavaScript DSL**: Produces a normalized, language-agnostic Intermediate Representation (IR).
- **Validation**: Strict validation pass in Rust verifying column definitions, foreign keys, uniqueness, primary keys, and backend capabilities.
- **Emitters**: Read the validated IR and emit code independently.
- **Migrations**: Compute differences between committed schema snapshots (`schema.lock.json`) and the current IR to produce `up.sql` and `down.sql`.

---

## Type Mappings

| Logical Type  | SQLite                                          | PostgreSQL                | Rust                    | Diesel Type                      |
| ------------- | ----------------------------------------------- | ------------------------- | ----------------------- | -------------------------------- |
| `integer`     | `INTEGER` / `INTEGER PRIMARY KEY AUTOINCREMENT` | `INTEGER` / `SERIAL`      | `i32`                   | `diesel::sql_types::Integer`     |
| `bigint`      | `INTEGER`                                       | `BIGINT` / `BIGSERIAL`    | `i64`                   | `diesel::sql_types::BigInt`      |
| `float`       | `REAL`                                          | `REAL`                    | `f32`                   | `diesel::sql_types::Float`       |
| `double`      | `REAL`                                          | `DOUBLE PRECISION`        | `f64`                   | `diesel::sql_types::Double`      |
| `boolean`     | `INTEGER` (0 or 1)                              | `BOOLEAN`                 | `bool`                  | `diesel::sql_types::Bool`        |
| `text`        | `TEXT`                                          | `TEXT`                    | `String`                | `diesel::sql_types::Text`        |
| `timestamp`   | `TEXT`                                          | `TIMESTAMP`               | `chrono::NaiveDateTime` | `diesel::sql_types::Timestamp`   |
| `timestamptz` | `TEXT`                                          | `TIMESTAMPTZ`             | `chrono::DateTime<Utc>` | `diesel::sql_types::Timestamptz` |
| `uuid`        | `TEXT`                                          | `UUID`                    | `uuid::Uuid`            | `diesel::sql_types::Uuid`        |
| `json`        | `TEXT`                                          | `JSONB`                   | `serde_json::Value`     | `diesel::sql_types::Jsonb`       |
| `enum`        | `TEXT` + `CHECK` constraint                     | `CREATE TYPE ... AS ENUM` | Generated Rust `enum`   | Generated SQL enum               |

---

## Core NAPI Functions

### `compileSchema(schemaJson: string, backendName?: string): GeneratedArtifacts`

Validates the input Schema IR JSON against the selected backend (`"sqlite"` or `"postgres"`) and emits all code artifacts.

#### Return Value (`GeneratedArtifacts`)

- `rustModels`: Rust Row struct, `NewX` insert struct, and enum definitions.
- `dieselSchema`: `diesel::table!`, `diesel::joinable!`, and `diesel::allow_tables_to_appear_in_same_query!`.
- `sqlDdl`: DDL (`CREATE TABLE`, enums, indexes, constraints).
- `napiBindings`: `#[napi(object)]` struct representations.
- `validators`: Runtime `validate(&self) -> Result<(), Vec<String>>` methods enforcing `maxLength` and text constraints.

### `validateSchemaJson(schemaJson: string, backendName?: string): boolean`

Performs strict validation of a Schema IR JSON string against integrity rules and backend capabilities. Throws on validation failure with a descriptive error list.

### `generateMigration(oldSchemaJson: string, newSchemaJson: string, backendName?: string): GeneratedMigration`

Diffs two Schema IR snapshots and outputs migration scripts.

#### Return Value (`GeneratedMigration`)

- `upSql`: SQL statements to transition to the new schema.
- `downSql`: SQL statements to revert the changes.
- `changesCount`: Total number of detected schema operations.

#### SQLite Table Rebuilds

For SQLite, changes to column types or nullability trigger the standard table-rebuild sequence:

1. `PRAGMA foreign_keys = OFF;`
2. `BEGIN TRANSACTION;`
3. `CREATE TABLE _<table>_new (...)`
4. `INSERT INTO _<table>_new (...) SELECT ... FROM <table>;`
5. `DROP TABLE <table>;`
6. `ALTER TABLE _<table>_new RENAME TO <table>;`
7. `PRAGMA foreign_key_check;`
8. `COMMIT; PRAGMA foreign_keys = ON;`

---

## Example Schema IR

```json
{
  "irVersion": 1,
  "naming": {
    "tableStrategy": "snake_case",
    "columnStrategy": "snake_case",
    "structStrategy": "PascalCase"
  },
  "enums": [
    {
      "name": "user_role",
      "values": ["admin", "editor", "viewer"]
    }
  ],
  "tables": [
    {
      "name": "users",
      "columns": [
        {
          "key": "id",
          "name": "id",
          "type": { "kind": "integer" },
          "nullable": false,
          "autoIncrement": true
        },
        {
          "key": "name",
          "name": "name",
          "type": { "kind": "text" },
          "nullable": false,
          "maxLength": 100
        },
        {
          "key": "role",
          "name": "role",
          "type": { "kind": "enum", "name": "user_role" },
          "nullable": false,
          "default": { "kind": "literal", "value": "viewer" }
        }
      ],
      "primaryKey": ["id"],
      "uniques": [],
      "indexes": [],
      "foreignKeys": []
    }
  ]
}
```

---

## SQL QueryDsl & RunQueryDsl (`src/sql/query.rs`)

The `query` module provides chainable SQL query building and execution via SQLite connection and transaction wrappers.

### `SqlConnection`

```ts
class SqlConnection {
  constructor(pathOrUrl: string);
  execute(sql: string): number;
  executeBatch(sql: string): void;
  query(table: string): SqlQueryBuilder;
  transaction<T>(callback: (tx: SqlTransaction) => T): T;
}
```

### `SqlQueryBuilder`

Provides chainable query construction:

- `.select(columns: string[])`: Limits columns selected in the query.
- `.distinct()`: Adds `DISTINCT` clause.
- `.filterEq(column: string, value: any)`: Equality predicate (`col = val`).
- `.filterNe(column: string, value: any)`: Inequality predicate (`col != val`).
- `.filterGt(column: string, value: any)` / `.filterLt(column, value)`: Greater/less than predicates.
- `.filterLike(column: string, pattern: string)`: SQL `LIKE` filtering.
- `.filterIn(column: string, values: any[])`: SQL `IN (...)` filtering.
- `.filterIsNull(column: string)` / `.filterIsNotNull(column: string)`: Nullness check predicates.
- `.orderByAsc(column: string)` / `.orderByDesc(column: string)`: Ordering.
- `.limit(n: number)` / `.offset(n: number)`: Pagination.
- `.insert(record: Record<string, any>)`: Inserts record and returns modified rows.
- `.update(record: Record<string, any>)`: Updates matched records.
- `.delete()`: Deletes matched records.
- `.load()`: Executes query and returns array of row objects.
- `.first()`: Returns first row object or `null`.
- `.getResult()`: Returns exactly one row object (throws if zero or multiple).
- `.count()`: Returns number of matching rows.
- `.exists()`: Returns boolean indicating existence of matching rows.
- `.toSql()`: Returns generated SQL string for debugging.
