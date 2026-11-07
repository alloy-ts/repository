//! Dynamic Query DSL and RunQueryDsl NAPI bindings over Diesel SQL.
//!
//! Provides a flexible, chainable query builder mirroring Diesel's:
//! - [`diesel::query_dsl::QueryDsl`](https://docs.diesel.rs/main/diesel/query_dsl/trait.QueryDsl.html)
//!   (select, filter, order_by, limit, offset, distinct)
//! - [`diesel::query_dsl::RunQueryDsl`](https://docs.diesel.rs/main/diesel/query_dsl/trait.RunQueryDsl.html)
//!   (execute, load, first, get_result, get_results)
//!
//! Backed by SQLite connections with parameter binding, transaction support, and JSON result mappings.

use std::sync::{Arc, Mutex};

use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sqlite::Sqlite;
use diesel::sqlite::SqliteConnection;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde_json::Value;

use crate::error::{err, err_str};

/// A SQL connection wrapper for query execution.
#[napi(js_name = "SqlConnection")]
pub struct JsSqlConnection {
    pub(crate) conn: Arc<Mutex<SqliteConnection>>,
}

#[napi]
impl JsSqlConnection {
    /// Open a new connection (e.g. `":memory:"` or a file path).
    #[napi(constructor)]
    pub fn new(database_url: String) -> Result<Self> {
        let mut conn =
            SqliteConnection::establish(&database_url).map_err(|e| err("SqlConnection::new", e))?;
        conn.batch_execute(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            "#,
        )
        .map_err(|e| err("SqlConnection::new", e))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Execute raw DDL or batch SQL statements (mirrors `SimpleConnection::batch_execute`).
    #[napi]
    pub fn execute_batch(&self, sql: String) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        conn.batch_execute(&sql)
            .map_err(|e| err("execute_batch", e))
    }

    /// Start a new query on the specified table (mirrors `table.into_boxed()`).
    #[napi]
    pub fn query(&self, table: String) -> JsQueryBuilder {
        JsQueryBuilder::new(self.conn.clone(), table)
    }

    /// Execute a raw SQL query with parameters and return JSON rows (mirrors `diesel::sql_query`).
    #[napi]
    pub fn execute_raw(&self, sql: String) -> Result<u32> {
        use diesel::sql_query;
        let mut conn = self.conn.lock().unwrap();
        let rows = sql_query(sql)
            .execute(&mut *conn)
            .map_err(|e| err("execute_raw", e))?;
        Ok(rows as u32)
    }
}

/// Where condition representation.
#[derive(Clone, Debug)]
pub enum Condition {
    Eq(String, Value),
    Ne(String, Value),
    Gt(String, Value),
    Gte(String, Value),
    Lt(String, Value),
    Lte(String, Value),
    Like(String, String),
    IsNull(String),
    IsNotNull(String),
    In(String, Vec<Value>),
    Raw(String),
}

/// Order by direction.
#[derive(Clone, Debug)]
pub enum OrderDirection {
    Asc,
    Desc,
}

/// Chainable query builder implementing Diesel QueryDsl and RunQueryDsl operations.
#[napi(js_name = "QueryBuilder")]
#[derive(Clone)]
pub struct JsQueryBuilder {
    conn: Arc<Mutex<SqliteConnection>>,
    table: String,
    select_fields: Vec<String>,
    distinct: bool,
    conditions: Vec<Condition>,
    order_by: Vec<(String, OrderDirection)>,
    limit_val: Option<i64>,
    offset_val: Option<i64>,
}

impl JsQueryBuilder {
    pub(crate) fn new(conn: Arc<Mutex<SqliteConnection>>, table: String) -> Self {
        Self {
            conn,
            table,
            select_fields: Vec::new(),
            distinct: false,
            conditions: Vec::new(),
            order_by: Vec::new(),
            limit_val: None,
            offset_val: None,
        }
    }

    /// Internal SQL renderer.
    fn to_sql_internal(&self) -> String {
        let select_clause = if self.select_fields.is_empty() {
            "*".to_string()
        } else {
            self.select_fields.join(", ")
        };

        let distinct_clause = if self.distinct { "DISTINCT " } else { "" };
        let mut sql = format!(
            "SELECT {distinct_clause}{select_clause} FROM {}",
            self.table
        );

        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            let cond_strings: Vec<String> = self
                .conditions
                .iter()
                .map(|c| match c {
                    Condition::Eq(col, val) => format!("{col} = {}", json_to_sql_literal(val)),
                    Condition::Ne(col, val) => format!("{col} != {}", json_to_sql_literal(val)),
                    Condition::Gt(col, val) => format!("{col} > {}", json_to_sql_literal(val)),
                    Condition::Gte(col, val) => format!("{col} >= {}", json_to_sql_literal(val)),
                    Condition::Lt(col, val) => format!("{col} < {}", json_to_sql_literal(val)),
                    Condition::Lte(col, val) => format!("{col} <= {}", json_to_sql_literal(val)),
                    Condition::Like(col, pattern) => {
                        format!("{col} LIKE '{}'", pattern.replace('\'', "''"))
                    }
                    Condition::IsNull(col) => format!("{col} IS NULL"),
                    Condition::IsNotNull(col) => format!("{col} IS NOT NULL"),
                    Condition::In(col, vals) => {
                        let lits: Vec<String> = vals.iter().map(json_to_sql_literal).collect();
                        format!("{col} IN ({})", lits.join(", "))
                    }
                    Condition::Raw(raw) => format!("({raw})"),
                })
                .collect();
            sql.push_str(&cond_strings.join(" AND "));
        }

        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            let orders: Vec<String> = self
                .order_by
                .iter()
                .map(|(col, dir)| match dir {
                    OrderDirection::Asc => format!("{col} ASC"),
                    OrderDirection::Desc => format!("{col} DESC"),
                })
                .collect();
            sql.push_str(&orders.join(", "));
        }

        if let Some(lim) = self.limit_val {
            sql.push_str(&format!(" LIMIT {lim}"));
        }
        if let Some(off) = self.offset_val {
            sql.push_str(&format!(" OFFSET {off}"));
        }

        sql
    }

    fn get_columns(&self, conn: &mut SqliteConnection) -> Result<Vec<String>> {
        if !self.select_fields.is_empty() {
            return Ok(self.select_fields.clone());
        }

        let pragma_sql = format!("PRAGMA table_info({})", self.table);
        let columns: Vec<TableInfoRow> = diesel::sql_query(pragma_sql)
            .load::<TableInfoRow>(conn)
            .map_err(|e| err("get_columns", e))?;

        Ok(columns.into_iter().map(|c| c.name).collect())
    }

    fn execute_select_json(&self) -> Result<Vec<Value>> {
        let mut conn = self.conn.lock().unwrap();
        let cols = self.get_columns(&mut *conn)?;

        let json_obj_args: Vec<String> = cols.iter().map(|col| format!("'{col}', {col}")).collect();
        let json_obj = if json_obj_args.is_empty() {
            "json_object()".to_string()
        } else {
            format!("json_object({})", json_obj_args.join(", "))
        };

        let query = self.to_sql_internal();
        let json_sql = format!("SELECT json_group_array({json_obj}) AS json_str FROM ({query})");

        let res: Option<JsonRow> = diesel::sql_query(json_sql)
            .get_result::<JsonRow>(&mut *conn)
            .optional()
            .map_err(|e| err("execute_select", e))?;

        match res.and_then(|r| r.json_str) {
            Some(s) if !s.is_empty() => serde_json::from_str::<Vec<Value>>(&s)
                .map_err(|e| err("execute_select: json parse error", e)),
            _ => Ok(Vec::new()),
        }
    }
}

#[derive(QueryableByName)]
#[diesel(check_for_backend(Sqlite))]
struct JsonRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    json_str: Option<String>,
}

#[derive(QueryableByName)]
#[diesel(check_for_backend(Sqlite))]
struct TableInfoRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

#[derive(QueryableByName)]
#[diesel(check_for_backend(Sqlite))]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

#[derive(QueryableByName)]
#[diesel(check_for_backend(Sqlite))]
struct ExistsRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    exists_flag: i32,
}

fn json_to_sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_sql_literal).collect();
            format!("'[{}]'", items.join(","))
        }
        Value::Object(_) => format!(
            "'{}'",
            serde_json::to_string(v).unwrap().replace('\'', "''")
        ),
    }
}

#[napi]
impl JsQueryBuilder {
    // -------------------------------------------------------------------------
    // QueryDsl operations (chainable query transformations)
    // -------------------------------------------------------------------------

    /// Select specific fields (mirrors `QueryDsl::select`).
    #[napi]
    pub fn select(&self, fields: Vec<String>) -> Self {
        let mut cloned = self.clone();
        cloned.select_fields = fields;
        cloned
    }

    /// Add DISTINCT modifier (mirrors `QueryDsl::distinct`).
    #[napi]
    pub fn distinct(&self) -> Self {
        let mut cloned = self.clone();
        cloned.distinct = true;
        cloned
    }

    /// Filter by column equality: `column = value` (mirrors `QueryDsl::filter`).
    #[napi]
    pub fn filter_eq(&self, column: String, value: Value) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Eq(column, value));
        cloned
    }

    /// Filter by column inequality: `column != value` (mirrors `QueryDsl::filter`).
    #[napi]
    pub fn filter_ne(&self, column: String, value: Value) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Ne(column, value));
        cloned
    }

    /// Filter `column > value` (mirrors `QueryDsl::filter`).
    #[napi]
    pub fn filter_gt(&self, column: String, value: Value) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Gt(column, value));
        cloned
    }

    /// Filter `column >= value` (mirrors `QueryDsl::filter`).
    #[napi]
    pub fn filter_gte(&self, column: String, value: Value) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Gte(column, value));
        cloned
    }

    /// Filter `column < value` (mirrors `QueryDsl::filter`).
    #[napi]
    pub fn filter_lt(&self, column: String, value: Value) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Lt(column, value));
        cloned
    }

    /// Filter `column <= value` (mirrors `QueryDsl::filter`).
    #[napi]
    pub fn filter_lte(&self, column: String, value: Value) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Lte(column, value));
        cloned
    }

    /// Filter `column LIKE pattern` (mirrors `ExpressionMethods::like`).
    #[napi]
    pub fn filter_like(&self, column: String, pattern: String) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Like(column, pattern));
        cloned
    }

    /// Filter `column IS NULL` (mirrors `ExpressionMethods::is_null`).
    #[napi]
    pub fn filter_is_null(&self, column: String) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::IsNull(column));
        cloned
    }

    /// Filter `column IS NOT NULL` (mirrors `ExpressionMethods::is_not_null`).
    #[napi]
    pub fn filter_is_not_null(&self, column: String) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::IsNotNull(column));
        cloned
    }

    /// Filter `column IN (...)` (mirrors `ExpressionMethods::eq_any`).
    #[napi]
    pub fn filter_in(&self, column: String, values: Vec<Value>) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::In(column, values));
        cloned
    }

    /// Filter with raw SQL condition snippet.
    #[napi]
    pub fn filter_raw(&self, raw_sql: String) -> Self {
        let mut cloned = self.clone();
        cloned.conditions.push(Condition::Raw(raw_sql));
        cloned
    }

    /// Order ascending by column (mirrors `QueryDsl::order` / `col.asc()`).
    #[napi]
    pub fn order_by_asc(&self, column: String) -> Self {
        let mut cloned = self.clone();
        cloned.order_by.push((column, OrderDirection::Asc));
        cloned
    }

    /// Order descending by column (mirrors `QueryDsl::order` / `col.desc()`).
    #[napi]
    pub fn order_by_desc(&self, column: String) -> Self {
        let mut cloned = self.clone();
        cloned.order_by.push((column, OrderDirection::Desc));
        cloned
    }

    /// Limit maximum returned rows (mirrors `QueryDsl::limit`).
    #[napi]
    pub fn limit(&self, limit: f64) -> Self {
        let mut cloned = self.clone();
        cloned.limit_val = Some(limit as i64);
        cloned
    }

    /// Offset returned rows (mirrors `QueryDsl::offset`).
    #[napi]
    pub fn offset(&self, offset: f64) -> Self {
        let mut cloned = self.clone();
        cloned.offset_val = Some(offset as i64);
        cloned
    }

    /// Return the rendered SQL string (mirrors `diesel::debug_query`).
    #[napi]
    pub fn to_sql(&self) -> String {
        self.to_sql_internal()
    }

    // -------------------------------------------------------------------------
    // RunQueryDsl operations (query execution)
    // -------------------------------------------------------------------------

    /// Load all matching records as a JSON array (mirrors `RunQueryDsl::load`).
    #[napi]
    pub fn load(&self) -> Result<Value> {
        let items = self.execute_select_json()?;
        Ok(Value::Array(items))
    }

    /// Return the first matching record or null (mirrors `RunQueryDsl::first`).
    #[napi]
    pub fn first(&self) -> Result<Option<Value>> {
        let mut limited = self.clone();
        limited.limit_val = Some(1);
        let items = limited.execute_select_json()?;
        Ok(items.into_iter().next())
    }

    /// Get single result or throw if not found (mirrors `RunQueryDsl::get_result`).
    #[napi]
    pub fn get_result(&self) -> Result<Value> {
        let item = self.first()?;
        item.ok_or_else(|| err_str("get_result", "Record not found (NotFound)"))
    }

    /// Count matching rows (mirrors `QueryDsl::count`).
    #[napi]
    pub fn count(&self) -> Result<f64> {
        let mut count_query = self.clone();
        count_query.select_fields = vec!["COUNT(*) AS count".to_string()];
        count_query.limit_val = None;
        count_query.offset_val = None;
        count_query.order_by.clear();

        let sql = count_query.to_sql_internal();
        let mut conn = self.conn.lock().unwrap();
        let row: CountRow = diesel::sql_query(sql)
            .get_result(&mut *conn)
            .map_err(|e| err("count", e))?;

        Ok(row.count as f64)
    }

    /// Check if at least one matching row exists (mirrors `diesel::dsl::exists`).
    #[napi]
    pub fn exists(&self) -> Result<bool> {
        let subquery = self.to_sql_internal();
        let sql = format!("SELECT EXISTS({subquery}) AS exists_flag");
        let mut conn = self.conn.lock().unwrap();
        let row: ExistsRow = diesel::sql_query(sql)
            .get_result(&mut *conn)
            .map_err(|e| err("exists", e))?;

        Ok(row.exists_flag != 0)
    }

    /// Insert a record into this table (mirrors `diesel::insert_into`).
    #[napi]
    pub fn insert(&self, record: Value) -> Result<u32> {
        let obj = record
            .as_object()
            .ok_or_else(|| err_str("insert", "insert record must be an object"))?;

        if obj.is_empty() {
            return Err(err_str("insert", "cannot insert empty object"));
        }

        let cols: Vec<String> = obj.keys().cloned().collect();
        let vals: Vec<String> = obj.values().map(json_to_sql_literal).collect();

        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table,
            cols.join(", "),
            vals.join(", ")
        );

        let mut conn = self.conn.lock().unwrap();
        let affected = diesel::sql_query(sql)
            .execute(&mut *conn)
            .map_err(|e| err("insert", e))?;

        Ok(affected as u32)
    }

    /// Update records matching this query with the given values (mirrors `diesel::update`).
    #[napi]
    pub fn update(&self, values: Value) -> Result<u32> {
        let obj = values
            .as_object()
            .ok_or_else(|| err_str("update", "update values must be an object"))?;

        if obj.is_empty() {
            return Ok(0);
        }

        let sets: Vec<String> = obj
            .iter()
            .map(|(k, v)| format!("{k} = {}", json_to_sql_literal(v)))
            .collect();

        let mut sql = format!("UPDATE {} SET {}", self.table, sets.join(", "));

        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            let cond_strings: Vec<String> = self
                .conditions
                .iter()
                .map(|c| match c {
                    Condition::Eq(col, val) => format!("{col} = {}", json_to_sql_literal(val)),
                    Condition::Ne(col, val) => format!("{col} != {}", json_to_sql_literal(val)),
                    Condition::Gt(col, val) => format!("{col} > {}", json_to_sql_literal(val)),
                    Condition::Gte(col, val) => format!("{col} >= {}", json_to_sql_literal(val)),
                    Condition::Lt(col, val) => format!("{col} < {}", json_to_sql_literal(val)),
                    Condition::Lte(col, val) => format!("{col} <= {}", json_to_sql_literal(val)),
                    Condition::Like(col, pattern) => {
                        format!("{col} LIKE '{}'", pattern.replace('\'', "''"))
                    }
                    Condition::IsNull(col) => format!("{col} IS NULL"),
                    Condition::IsNotNull(col) => format!("{col} IS NOT NULL"),
                    Condition::In(col, vals) => {
                        let lits: Vec<String> = vals.iter().map(json_to_sql_literal).collect();
                        format!("{col} IN ({})", lits.join(", "))
                    }
                    Condition::Raw(raw) => format!("({raw})"),
                })
                .collect();
            sql.push_str(&cond_strings.join(" AND "));
        }

        let mut conn = self.conn.lock().unwrap();
        let affected = diesel::sql_query(sql)
            .execute(&mut *conn)
            .map_err(|e| err("update", e))?;

        Ok(affected as u32)
    }

    /// Delete records matching this query (mirrors `diesel::delete`).
    #[napi]
    pub fn delete(&self) -> Result<u32> {
        let mut sql = format!("DELETE FROM {}", self.table);

        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            let cond_strings: Vec<String> = self
                .conditions
                .iter()
                .map(|c| match c {
                    Condition::Eq(col, val) => format!("{col} = {}", json_to_sql_literal(val)),
                    Condition::Ne(col, val) => format!("{col} != {}", json_to_sql_literal(val)),
                    Condition::Gt(col, val) => format!("{col} > {}", json_to_sql_literal(val)),
                    Condition::Gte(col, val) => format!("{col} >= {}", json_to_sql_literal(val)),
                    Condition::Lt(col, val) => format!("{col} < {}", json_to_sql_literal(val)),
                    Condition::Lte(col, val) => format!("{col} <= {}", json_to_sql_literal(val)),
                    Condition::Like(col, pattern) => {
                        format!("{col} LIKE '{}'", pattern.replace('\'', "''"))
                    }
                    Condition::IsNull(col) => format!("{col} IS NULL"),
                    Condition::IsNotNull(col) => format!("{col} IS NOT NULL"),
                    Condition::In(col, vals) => {
                        let lits: Vec<String> = vals.iter().map(json_to_sql_literal).collect();
                        format!("{col} IN ({})", lits.join(", "))
                    }
                    Condition::Raw(raw) => format!("({raw})"),
                })
                .collect();
            sql.push_str(&cond_strings.join(" AND "));
        }

        let mut conn = self.conn.lock().unwrap();
        let affected = diesel::sql_query(sql)
            .execute(&mut *conn)
            .map_err(|e| err("delete", e))?;

        Ok(affected as u32)
    }
}
