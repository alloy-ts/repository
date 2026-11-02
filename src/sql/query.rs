//! Diesel `QueryDsl` and `RunQueryDsl` NAPI bindings for dynamic query building and execution.
//!
//! Exposes an opaque database architecture where JavaScript interacts with `Database` and `Transaction`
//! structs wrapping Diesel SQLite connections. Provides dynamic, fluent AST query construction mirroring
//! Diesel's [`QueryDsl`](https://docs.diesel.rs/main/diesel/query_dsl/trait.QueryDsl.html) and query execution
//! mirroring [`RunQueryDsl`](https://docs.diesel.rs/main/diesel/query_dsl/trait.RunQueryDsl.html).

use std::sync::{Arc, Mutex};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde_json::Value;

/// Map a Diesel error into a descriptive NAPI error.
pub fn diesel_to_napi(err: diesel::result::Error) -> napi::Error {
    match err {
        diesel::result::Error::NotFound => {
            napi::Error::new(napi::Status::GenericFailure, "Record not found (NotFound)")
        }
        diesel::result::Error::DatabaseError(kind, info) => {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("DatabaseError({:?}): {}", kind, info.message()),
            )
        }
        diesel::result::Error::AlreadyInTransaction => {
            napi::Error::new(napi::Status::GenericFailure, "Transaction already active")
        }
        diesel::result::Error::NotInTransaction => {
            napi::Error::new(napi::Status::GenericFailure, "No active transaction")
        }
        other => napi::Error::new(napi::Status::GenericFailure, format!("Diesel error: {other}")),
    }
}

/// Helper converting a SQLite connection error into NAPI error.
pub fn connection_to_napi(err: diesel::ConnectionError) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, format!("Connection error: {err}"))
}

/// Sort direction for query ordering.
#[napi(string_enum)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDirection {
    Asc,
    Desc,
}

/// Join type for table joins.
#[napi(string_enum)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    Left,
}

/// Binary comparison operators supported in filters.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Like,
    NotLike,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

/// A filter predicate representing a WHERE condition.
#[derive(Debug, Clone)]
pub struct FilterPredicate {
    pub column: String,
    pub operator: FilterOperator,
    pub value: Value,
    pub is_or: bool,
}

/// Ordering clause item.
#[derive(Debug, Clone)]
pub struct OrderItem {
    pub column: String,
    pub direction: OrderDirection,
}

/// Join specification.
#[derive(Debug, Clone)]
pub struct JoinSpec {
    pub join_type: JoinType,
    pub table: String,
    pub on_clause: Option<String>,
}

/// Dynamic Query builder mirroring Diesel's `QueryDsl` and `RunQueryDsl`.
#[napi(js_name = "QueryBuilder")]
#[derive(Clone, Debug)]
pub struct QueryBuilder {
    pub(crate) table_name: String,
    pub(crate) selections: Vec<String>,
    pub(crate) distinct: bool,
    pub(crate) distinct_on: Option<String>,
    pub(crate) filters: Vec<FilterPredicate>,
    pub(crate) orders: Vec<OrderItem>,
    pub(crate) limit_val: Option<i64>,
    pub(crate) offset_val: Option<i64>,
    pub(crate) group_by_cols: Vec<String>,
    pub(crate) having_clause: Option<String>,
    pub(crate) joins: Vec<JoinSpec>,
    pub(crate) is_count: bool,
}

#[napi]
impl QueryBuilder {
    /// Create a new query builder targeting a specific table.
    #[napi(constructor)]
    pub fn new(table_name: String) -> Self {
        Self {
            table_name,
            selections: Vec::new(),
            distinct: false,
            distinct_on: None,
            filters: Vec::new(),
            orders: Vec::new(),
            limit_val: None,
            offset_val: None,
            group_by_cols: Vec::new(),
            having_clause: None,
            joins: Vec::new(),
            is_count: false,
        }
    }

    /// `QueryDsl::select`: Sets the columns to be projected.
    #[napi]
    pub fn select(&self, columns: Vec<String>) -> Self {
        let mut q = self.clone();
        q.selections = columns;
        q
    }

    /// `QueryDsl::distinct`: Adds DISTINCT to the query.
    #[napi]
    pub fn distinct(&self) -> Self {
        let mut q = self.clone();
        q.distinct = true;
        q
    }

    /// `QueryDsl::distinct_on`: Adds DISTINCT ON clause.
    #[napi(js_name = "distinctOn")]
    pub fn distinct_on(&self, column: String) -> Self {
        let mut q = self.clone();
        q.distinct = true;
        q.distinct_on = Some(column);
        q
    }

    /// `QueryDsl::count`: Equivalent to `.select(["COUNT(*)"])`.
    #[napi]
    pub fn count(&self) -> Self {
        let mut q = self.clone();
        q.is_count = true;
        q
    }

    /// `QueryDsl::filter`: Adds an AND condition to the WHERE clause.
    #[napi]
    pub fn filter(&self, column: String, op: String, value: Option<serde_json::Value>) -> Result<Self> {
        let mut q = self.clone();
        let operator = parse_operator(&op)?;
        q.filters.push(FilterPredicate {
            column,
            operator,
            value: value.unwrap_or(Value::Null),
            is_or: false,
        });
        Ok(q)
    }

    /// `QueryDsl::or_filter`: Adds an OR condition to the WHERE clause.
    #[napi(js_name = "orFilter")]
    pub fn or_filter(&self, column: String, op: String, value: Option<serde_json::Value>) -> Result<Self> {
        let mut q = self.clone();
        let operator = parse_operator(&op)?;
        q.filters.push(FilterPredicate {
            column,
            operator,
            value: value.unwrap_or(Value::Null),
            is_or: true,
        });
        Ok(q)
    }

    /// `QueryDsl::find`: Finds a single record by primary key (`id = ?`).
    #[napi]
    pub fn find(&self, id: serde_json::Value) -> Self {
        let mut q = self.clone();
        q.filters.push(FilterPredicate {
            column: "id".to_string(),
            operator: FilterOperator::Eq,
            value: id,
            is_or: false,
        });
        q.limit_val = Some(1);
        q
    }

    /// `QueryDsl::order` / `QueryDsl::order_by`: Replaces the order clause.
    #[napi(js_name = "orderBy")]
    pub fn order_by(&self, column: String, direction: Option<OrderDirection>) -> Self {
        let mut q = self.clone();
        q.orders = vec![OrderItem {
            column,
            direction: direction.unwrap_or(OrderDirection::Asc),
        }];
        q
    }

    /// `QueryDsl::then_order_by`: Appends to the order clause.
    #[napi(js_name = "thenOrderBy")]
    pub fn then_order_by(&self, column: String, direction: Option<OrderDirection>) -> Self {
        let mut q = self.clone();
        q.orders.push(OrderItem {
            column,
            direction: direction.unwrap_or(OrderDirection::Asc),
        });
        q
    }

    /// `QueryDsl::limit`: Sets LIMIT on the query.
    #[napi]
    pub fn limit(&self, limit: i64) -> Self {
        let mut q = self.clone();
        q.limit_val = Some(limit);
        q
    }

    /// `QueryDsl::offset`: Sets OFFSET on the query.
    #[napi]
    pub fn offset(&self, offset: i64) -> Self {
        let mut q = self.clone();
        q.offset_val = Some(offset);
        q
    }

    /// `QueryDsl::group_by`: Sets GROUP BY columns.
    #[napi(js_name = "groupBy")]
    pub fn group_by(&self, columns: Vec<String>) -> Self {
        let mut q = self.clone();
        q.group_by_cols = columns;
        q
    }

    /// `QueryDsl::having`: Sets HAVING predicate expression.
    #[napi]
    pub fn having(&self, predicate: String) -> Self {
        let mut q = self.clone();
        q.having_clause = Some(predicate);
        q
    }

    /// `QueryDsl::inner_join`: Adds an INNER JOIN with optional ON clause.
    #[napi(js_name = "innerJoin")]
    pub fn inner_join(&self, table: String, on_clause: Option<String>) -> Self {
        let mut q = self.clone();
        q.joins.push(JoinSpec {
            join_type: JoinType::Inner,
            table,
            on_clause,
        });
        q
    }

    /// `QueryDsl::left_join`: Adds a LEFT OUTER JOIN with optional ON clause.
    #[napi(js_name = "leftJoin")]
    pub fn left_join(&self, table: String, on_clause: Option<String>) -> Self {
        let mut q = self.clone();
        q.joins.push(JoinSpec {
            join_type: JoinType::Left,
            table,
            on_clause,
        });
        q
    }

    /// Compile the query builder into raw SQL and parameters.
    #[napi(js_name = "toSql")]
    pub fn to_sql(&self) -> String {
        self.compile_sql()
    }
}

impl QueryBuilder {
    pub(crate) fn compile_sql(&self) -> String {
        let mut sql = String::new();
        sql.push_str("SELECT ");

        if self.distinct {
            if let Some(ref col) = self.distinct_on {
                sql.push_str(&format!("DISTINCT ON ({col}) "));
            } else {
                sql.push_str("DISTINCT ");
            }
        }

        if self.is_count {
            sql.push_str("COUNT(*)");
        } else if self.selections.is_empty() {
            sql.push('*');
        } else {
            sql.push_str(&self.selections.join(", "));
        }

        sql.push_str(" FROM ");
        sql.push_str(&self.table_name);

        for j in &self.joins {
            match j.join_type {
                JoinType::Inner => sql.push_str(" INNER JOIN "),
                JoinType::Left => sql.push_str(" LEFT JOIN "),
            }
            sql.push_str(&j.table);
            if let Some(ref on) = j.on_clause {
                sql.push_str(" ON ");
                sql.push_str(on);
            }
        }

        if !self.filters.is_empty() {
            sql.push_str(" WHERE ");
            for (i, f) in self.filters.iter().enumerate() {
                if i > 0 {
                    if f.is_or {
                        sql.push_str(" OR ");
                    } else {
                        sql.push_str(" AND ");
                    }
                }
                sql.push_str(&format_filter(f));
            }
        }

        if !self.group_by_cols.is_empty() {
            sql.push_str(" GROUP BY ");
            sql.push_str(&self.group_by_cols.join(", "));
        }

        if let Some(ref having) = self.having_clause {
            sql.push_str(" HAVING ");
            sql.push_str(having);
        }

        if !self.orders.is_empty() {
            sql.push_str(" ORDER BY ");
            let order_strs: Vec<String> = self
                .orders
                .iter()
                .map(|o| {
                    let dir = match o.direction {
                        OrderDirection::Asc => "ASC",
                        OrderDirection::Desc => "DESC",
                    };
                    format!("{} {}", o.column, dir)
                })
                .collect();
            sql.push_str(&order_strs.join(", "));
        }

        if let Some(limit) = self.limit_val {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        if let Some(offset) = self.offset_val {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        sql
    }
}

fn parse_operator(op: &str) -> Result<FilterOperator> {
    match op.trim().to_lowercase().as_str() {
        "=" | "eq" => Ok(FilterOperator::Eq),
        "!=" | "<>" | "ne" => Ok(FilterOperator::Ne),
        ">" | "gt" => Ok(FilterOperator::Gt),
        ">=" | "gte" => Ok(FilterOperator::Gte),
        "<" | "lt" => Ok(FilterOperator::Lt),
        "<=" | "lte" => Ok(FilterOperator::Lte),
        "like" => Ok(FilterOperator::Like),
        "not_like" | "not like" => Ok(FilterOperator::NotLike),
        "in" => Ok(FilterOperator::In),
        "not_in" | "not in" => Ok(FilterOperator::NotIn),
        "is_null" | "is null" => Ok(FilterOperator::IsNull),
        "is_not_null" | "is not null" => Ok(FilterOperator::IsNotNull),
        other => Err(Error::from_reason(format!("Unsupported filter operator: '{other}'"))),
    }
}

fn sanitize_ident(ident: &str) -> String {
    let clean: String = ident.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
    if clean.contains('.') {
        clean.split('.').map(|part| format!("\"{part}\"")).collect::<Vec<_>>().join(".")
    } else {
        format!("\"{clean}\"")
    }
}

fn format_value_sql(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let escaped = s.replace('\0', "").replace('\'', "''");
            format!("'{escaped}'")
        }
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_value_sql).collect();
            format!("({})", items.join(", "))
        }
        Value::Object(_) => {
            let s = v.to_string().replace('\0', "").replace('\'', "''");
            format!("'{s}'")
        }
    }
}

fn format_filter(f: &FilterPredicate) -> String {
    match f.operator {
        FilterOperator::Eq => format!("{} = {}", f.column, format_value_sql(&f.value)),
        FilterOperator::Ne => format!("{} <> {}", f.column, format_value_sql(&f.value)),
        FilterOperator::Gt => format!("{} > {}", f.column, format_value_sql(&f.value)),
        FilterOperator::Gte => format!("{} >= {}", f.column, format_value_sql(&f.value)),
        FilterOperator::Lt => format!("{} < {}", f.column, format_value_sql(&f.value)),
        FilterOperator::Lte => format!("{} <= {}", f.column, format_value_sql(&f.value)),
        FilterOperator::Like => format!("{} LIKE {}", f.column, format_value_sql(&f.value)),
        FilterOperator::NotLike => format!("{} NOT LIKE {}", f.column, format_value_sql(&f.value)),
        FilterOperator::In => {
            if f.value.is_array() {
                format!("{} IN {}", f.column, format_value_sql(&f.value))
            } else {
                format!("{} IN ({})", f.column, format_value_sql(&f.value))
            }
        }
        FilterOperator::NotIn => {
            if f.value.is_array() {
                format!("{} NOT IN {}", f.column, format_value_sql(&f.value))
            } else {
                format!("{} NOT IN ({})", f.column, format_value_sql(&f.value))
            }
        }
        FilterOperator::IsNull => format!("{} IS NULL", f.column),
        FilterOperator::IsNotNull => format!("{} IS NOT NULL", f.column),
    }
}

/// Opaque Database connection wrapper managing an underlying Diesel SQLite connection.
#[napi(js_name = "Database")]
pub struct Database {
    conn: Arc<Mutex<SqliteConnection>>,
}

/// Opaque Transaction wrapper representing an in-flight SQLite transaction.
#[napi(js_name = "Transaction")]
pub struct Transaction {
    conn: Arc<Mutex<SqliteConnection>>,
    committed: Arc<Mutex<bool>>,
}

#[napi]
impl Database {
    /// Open a connection to a SQLite database by path (or `":memory:"`).
    #[napi(constructor)]
    pub fn new(url: String) -> Result<Self> {
        let conn = SqliteConnection::establish(&url).map_err(connection_to_napi)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Execute a raw SQL statement or batch (mirrors `RunQueryDsl::execute`).
    #[napi]
    pub fn execute(&self, sql: String) -> Result<i64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        conn.batch_execute(&sql)
            .map_err(diesel_to_napi)?;
        Ok(1)
    }

    /// `RunQueryDsl::load`: Execute a query and return all matching rows as JSON values.
    #[napi]
    pub fn load(&self, query: &QueryBuilder) -> Result<serde_json::Value> {
        let sql = query.compile_sql();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        execute_select_json(&mut conn, &sql)
    }

    /// `RunQueryDsl::first`: Execute a query with limit 1, returning the first row or throwing NotFound.
    #[napi]
    pub fn first(&self, query: &QueryBuilder) -> Result<serde_json::Value> {
        let mut q = query.clone();
        q.limit_val = Some(1);
        let sql = q.compile_sql();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        let rows = execute_select_json(&mut conn, &sql)?;
        let arr = rows
            .as_array()
            .ok_or_else(|| Error::from_reason("Expected array of rows"))?;
        if arr.is_empty() {
            return Err(diesel_to_napi(diesel::result::Error::NotFound));
        }
        Ok(arr[0].clone())
    }

    /// `RunQueryDsl::get_result`: Execute a statement and return the single affected/returned row.
    #[napi(js_name = "getResult")]
    pub fn get_result(&self, query: &QueryBuilder) -> Result<serde_json::Value> {
        self.first(query)
    }

    /// `RunQueryDsl::get_results`: Execute a statement and return all matching rows.
    #[napi(js_name = "getResults")]
    pub fn get_results(&self, query: &QueryBuilder) -> Result<serde_json::Value> {
        self.load(query)
    }

    /// Begin a transaction and return an opaque `Transaction` instance.
    #[napi(js_name = "beginTransaction")]
    pub fn begin_transaction(&self) -> Result<Transaction> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        conn.batch_execute("BEGIN IMMEDIATE TRANSACTION;")
            .map_err(diesel_to_napi)?;

        Ok(Transaction {
            conn: Arc::clone(&self.conn),
            committed: Arc::new(Mutex::new(false)),
        })
    }

    /// Helper for inserting a JSON record into a table.
    #[napi]
    pub fn insert(&self, table: String, values: serde_json::Value) -> Result<i64> {
        let obj = values
            .as_object()
            .ok_or_else(|| Error::from_reason("insert values must be an object"))?;

        let mut cols = Vec::new();
        let mut vals = Vec::new();

        for (k, v) in obj {
            cols.push(k.clone());
            vals.push(format_value_sql(v));
        }

        let sanitized_table = sanitize_ident(&table);
        let sanitized_cols: Vec<String> = cols.iter().map(|c| sanitize_ident(c)).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({});",
            sanitized_table,
            sanitized_cols.join(", "),
            vals.join(", ")
        );

        self.execute(sql)
    }
}

#[napi]
impl Transaction {
    /// Execute a command in this transaction (mirrors `RunQueryDsl::execute`).
    #[napi]
    pub fn execute(&self, sql: String) -> Result<i64> {
        self.ensure_active()?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        conn.batch_execute(&sql)
            .map_err(diesel_to_napi)?;
        Ok(1)
    }

    /// `RunQueryDsl::load`: Execute a query within this transaction.
    #[napi]
    pub fn load(&self, query: &QueryBuilder) -> Result<serde_json::Value> {
        self.ensure_active()?;
        let sql = query.compile_sql();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        execute_select_json(&mut conn, &sql)
    }

    /// `RunQueryDsl::first`: Load first row within this transaction or error with NotFound.
    #[napi]
    pub fn first(&self, query: &QueryBuilder) -> Result<serde_json::Value> {
        self.ensure_active()?;
        let mut q = query.clone();
        q.limit_val = Some(1);
        let sql = q.compile_sql();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        let rows = execute_select_json(&mut conn, &sql)?;
        let arr = rows
            .as_array()
            .ok_or_else(|| Error::from_reason("Expected array of rows"))?;
        if arr.is_empty() {
            return Err(diesel_to_napi(diesel::result::Error::NotFound));
        }
        Ok(arr[0].clone())
    }

    /// Commit the transaction.
    #[napi]
    pub fn commit(&self) -> Result<()> {
        self.ensure_active()?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        conn.batch_execute("COMMIT;")
            .map_err(diesel_to_napi)?;
        let mut committed = self
            .committed
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        *committed = true;
        Ok(())
    }

    /// Roll back the transaction.
    #[napi]
    pub fn rollback(&self) -> Result<()> {
        self.ensure_active()?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        conn.batch_execute("ROLLBACK;")
            .map_err(diesel_to_napi)?;
        let mut committed = self
            .committed
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        *committed = true;
        Ok(())
    }

    fn ensure_active(&self) -> Result<()> {
        let committed = self
            .committed
            .lock()
            .map_err(|e| Error::from_reason(format!("Mutex poisoned: {e}")))?;
        if *committed {
            return Err(diesel_to_napi(diesel::result::Error::NotInTransaction));
        }
        Ok(())
    }
}

/// Execute a raw SELECT query on a SQLite connection and serialize the result set into JSON.
fn execute_select_json(
    conn: &mut SqliteConnection,
    sql: &str,
) -> Result<serde_json::Value> {
    use diesel::connection::LoadConnection;
    use diesel::row::{Field, Row};

    let query = diesel::sql_query(sql);
    let mut row_iter = conn
        .load(query)
        .map_err(diesel_to_napi)?;

    let mut result_rows = Vec::new();

    while let Some(row) = row_iter.next() {
        let row = row.map_err(diesel_to_napi)?;
        let col_count = Row::field_count(&row);
        let mut obj = serde_json::Map::new();

        for i in 0..col_count {
            if let Some(field) = Row::get(&row, i) {
                let field_name = Field::field_name(&field)
                    .unwrap_or(&format!("col_{i}"))
                    .to_string();

                let val = if let Some(mut raw_val) = Field::value(&field) {
                    let s = raw_val.read_text();
                    if let Ok(int_val) = s.parse::<i64>() {
                        Value::Number(int_val.into())
                    } else if let Ok(f_val) = s.parse::<f64>() {
                        serde_json::Number::from_f64(f_val)
                            .map(Value::Number)
                            .unwrap_or(Value::String(s.to_string()))
                    } else {
                        Value::String(s.to_string())
                    }
                } else {
                    Value::Null
                };

                obj.insert(field_name, val);
            }
        }

        result_rows.push(Value::Object(obj));
    }

    Ok(Value::Array(result_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_builder_compilation() {
        let q = QueryBuilder::new("users".to_string())
            .select(vec!["id".to_string(), "name".to_string()])
            .filter("active".to_string(), "eq".to_string(), Some(serde_json::json!(true)))
            .unwrap()
            .or_filter("age".to_string(), "gte".to_string(), Some(serde_json::json!(18)))
            .unwrap()
            .order_by("name".to_string(), Some(OrderDirection::Asc))
            .then_order_by("id".to_string(), Some(OrderDirection::Desc))
            .limit(10)
            .offset(5);

        let sql = q.compile_sql();
        assert_eq!(
            sql,
            "SELECT id, name FROM users WHERE active = 1 OR age >= 18 ORDER BY name ASC, id DESC LIMIT 10 OFFSET 5"
        );
    }

    #[test]
    fn test_query_builder_joins_and_distinct() {
        let q = QueryBuilder::new("users".to_string())
            .distinct()
            .inner_join("posts".to_string(), Some("posts.user_id = users.id".to_string()))
            .left_join("comments".to_string(), Some("comments.post_id = posts.id".to_string()))
            .group_by(vec!["users.id".to_string()])
            .having("count(posts.id) > 1".to_string());

        let sql = q.compile_sql();
        assert_eq!(
            sql,
            "SELECT DISTINCT * FROM users INNER JOIN posts ON posts.user_id = users.id LEFT JOIN comments ON comments.post_id = posts.id GROUP BY users.id HAVING count(posts.id) > 1"
        );
    }

    #[test]
    fn test_database_and_transaction_lifecycle() {
        let db = Database::new(":memory:".to_string()).unwrap();
        db.execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1);".to_string()
        ).unwrap();

        db.insert("users".to_string(), serde_json::json!({ "id": 1, "name": "Sean", "active": true })).unwrap();
        db.insert("users".to_string(), serde_json::json!({ "id": 2, "name": "Tess", "active": false })).unwrap();

        let q = QueryBuilder::new("users".to_string());
        let rows = db.load(&q).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);

        let find_q = QueryBuilder::new("users".to_string()).find(serde_json::json!(1));
        let sean = db.first(&find_q).unwrap();
        assert_eq!(sean["name"], "Sean");

        // Transaction rollback test
        let tx = db.begin_transaction().unwrap();
        tx.execute("INSERT INTO users (id, name, active) VALUES (3, 'Ruby', 1);".to_string()).unwrap();
        tx.rollback().unwrap();

        // 3 should not exist
        let find_ruby = QueryBuilder::new("users".to_string()).find(serde_json::json!(3));
        assert!(db.first(&find_ruby).is_err());
    }

    #[test]
    fn test_diesel_to_napi_mapping() {
        let err = diesel_to_napi(diesel::result::Error::NotFound);
        assert!(err.reason.contains("NotFound"));
    }
}
