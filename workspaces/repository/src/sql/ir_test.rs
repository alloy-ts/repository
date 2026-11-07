// Shared by build.rs via #[path]. Typed IR + validation.
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

pub const SUPPORTED_IR_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Schema {
    pub ir_version: u32,
    pub enums: Vec<EnumDef>,
    pub tables: Vec<Table>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumDef {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub primary_key: Vec<String>,
    pub uniques: Vec<Constraint>,
    pub indexes: Vec<Constraint>,
    pub foreign_keys: Vec<ForeignKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Column {
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColType,
    pub nullable: bool,
    pub auto_increment: bool,
    pub max_length: Option<u32>,
    pub default: Option<Default>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ColType {
    Integer,
    Bigint,
    Float,
    Double,
    Text,
    Boolean,
    Timestamp,
    Timestamptz,
    Enum { name: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Default {
    Null,
    Now,
    Literal { value: serde_json::Value },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

pub fn validate(s: &Schema) -> Result<(), Vec<String>> {
    let mut e = Vec::new();
    if s.ir_version != SUPPORTED_IR_VERSION {
        return Err(vec![format!(
            "unsupported irVersion {} (expected {})",
            s.ir_version, SUPPORTED_IR_VERSION
        )]);
    }
    let enums: HashMap<_, _> = s.enums.iter().map(|x| (x.name.as_str(), x)).collect();
    let tables: HashMap<_, _> = s.tables.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut index_names = HashSet::new();

    for t in &s.tables {
        let cols: HashMap<_, _> = t.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        if cols.len() != t.columns.len() {
            e.push(format!("{}: duplicate column names", t.name));
        }
        let has = |n: &String| cols.contains_key(n.as_str());

        for c in &t.columns {
            let at = format!("{}.{}", t.name, c.name);
            let is_pk = t.primary_key.contains(&c.name);
            if is_pk && c.nullable {
                e.push(format!("{at}: primary key cannot be nullable"));
            }
            if c.auto_increment && !(is_pk && matches!(c.ty, ColType::Integer | ColType::Bigint)) {
                e.push(format!(
                    "{at}: autoIncrement needs an integer/bigint primary key"
                ));
            }
            if c.max_length.is_some() && c.ty != ColType::Text {
                e.push(format!("{at}: maxLength only applies to text"));
            }
            if let ColType::Enum { name } = &c.ty {
                match enums.get(name.as_str()) {
                    None => e.push(format!("{at}: enum \"{name}\" not declared in enums[]")),
                    Some(en) => {
                        if let Some(Default::Literal { value }) = &c.default {
                            if !value.as_str().is_some_and(|v| en.values.iter().any(|x| x == v)) {
                                e.push(format!(
                                    "{at}: default {value} is not a member of enum {name}"
                                ));
                            }
                        }
                    }
                }
            }
            if matches!(c.default, Some(Default::Now))
                && !matches!(c.ty, ColType::Timestamp | ColType::Timestamptz)
            {
                e.push(format!("{at}: defaultNow() only applies to timestamps"));
            }
        }
        if t.primary_key.is_empty() {
            e.push(format!("{}: no primary key", t.name));
        }
        for n in &t.primary_key {
            if !has(n) {
                e.push(format!("{}: primary key column \"{n}\" missing", t.name));
            }
        }
        for c in t.uniques.iter().chain(&t.indexes) {
            for n in &c.columns {
                if !has(n) {
                    e.push(format!(
                        "{}: {} references missing column \"{n}\"",
                        t.name, c.name
                    ));
                }
            }
        }
        for i in &t.indexes {
            if !index_names.insert(&i.name) {
                e.push(format!(
                    "index name \"{}\" is used twice (global in Postgres)",
                    i.name
                ));
            }
        }

        for fk in &t.foreign_keys {
            let Some(rt) = tables.get(fk.ref_table.as_str()) else {
                e.push(format!(
                    "{}: {} targets missing table \"{}\"",
                    t.name, fk.name, fk.ref_table
                ));
                continue;
            };
            if fk.columns.len() != fk.ref_columns.len() {
                e.push(format!("{}: {} column count mismatch", t.name, fk.name));
                continue;
            }
            for (l, r) in fk.columns.iter().zip(&fk.ref_columns) {
                let (Some(lc), Some(rc)) =
                    (cols.get(l.as_str()), rt.columns.iter().find(|c| &c.name == r))
                else {
                    e.push(format!("{}: {} references missing column", t.name, fk.name));
                    continue;
                };
                if lc.ty != rc.ty {
                    e.push(format!(
                        "{}: {} type mismatch {:?} vs {:?}",
                        t.name, fk.name, lc.ty, rc.ty
                    ));
                }
                let target_unique = rt.primary_key == [r.clone()]
                    || rt.uniques.iter().any(|u| u.columns == [r.clone()]);
                if !target_unique {
                    e.push(format!(
                        "{}: {} target {}.{} is not unique",
                        t.name, fk.name, rt.name, r
                    ));
                }
            }
        }
    }
    if e.is_empty() {
        Ok(())
    } else {
        Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn base() -> Value {
        json!({
          "irVersion": 1,
          "enums": [{ "name": "user_role", "values": ["admin", "viewer"] }],
          "tables": [
            { "name": "users",
              "columns": [
                { "key": "id", "name": "id", "type": {"kind": "integer"}, "nullable": false, "autoIncrement": true },
                { "key": "email", "name": "email", "type": {"kind": "text"}, "nullable": false, "autoIncrement": false },
                { "key": "age", "name": "age", "type": {"kind": "integer"}, "nullable": true, "autoIncrement": false },
                { "key": "name", "name": "name", "type": {"kind": "text"}, "nullable": false, "autoIncrement": false, "maxLength": 100 },
                { "key": "role", "name": "role", "type": {"kind": "enum", "name": "user_role"}, "nullable": false, "autoIncrement": false,
                  "default": {"kind": "literal", "value": "viewer"} },
                { "key": "createdAt", "name": "created_at", "type": {"kind": "timestamptz"}, "nullable": false, "autoIncrement": false,
                  "default": {"kind": "now"} }
              ],
              "primaryKey": ["id"],
              "uniques": [{ "name": "users_email_key", "columns": ["email"] }],
              "indexes": [{ "name": "users_email_idx", "columns": ["email"] }],
              "foreignKeys": [] },
            { "name": "posts",
              "columns": [
                { "key": "id", "name": "id", "type": {"kind": "integer"}, "nullable": false, "autoIncrement": true },
                { "key": "title", "name": "title", "type": {"kind": "text"}, "nullable": false, "autoIncrement": false },
                { "key": "authorId", "name": "author_id", "type": {"kind": "integer"}, "nullable": false, "autoIncrement": false }
              ],
              "primaryKey": ["id"],
              "uniques": [],
              "indexes": [{ "name": "posts_author_id_idx", "columns": ["author_id"] }],
              "foreignKeys": [{ "name": "posts_author_id_fkey", "columns": ["author_id"], "refTable": "users",
                                "refColumns": ["id"], "onDelete": "cascade", "onUpdate": "cascade" }] }
          ]
        })
    }

    fn set(v: &mut Value, ptr: &str, val: Value) {
        let (parent_ptr, field) = ptr.rsplit_once('/').expect("valid pointer");
        let parent = v.pointer_mut(parent_ptr).unwrap_or_else(|| panic!("no such path {parent_ptr}"));
        if let Some(map) = parent.as_object_mut() {
            map.insert(field.to_string(), val);
        } else if let Some(arr) = parent.as_array_mut() {
            let idx: usize = field.parse().expect("array index");
            arr[idx] = val;
        } else {
            panic!("parent at {parent_ptr} is neither object nor array");
        }
    }

    fn push(v: &mut Value, ptr: &str, val: Value) {
        v.pointer_mut(ptr).unwrap().as_array_mut().unwrap().push(val);
    }

    fn errors(v: &Value) -> Vec<String> {
        let s: Schema = serde_json::from_value(v.clone()).expect("IR should parse");
        validate(&s).err().unwrap_or_default()
    }

    fn assert_err(v: &Value, needle: &str) {
        let e = errors(v);
        assert!(e.iter().any(|m| m.contains(needle)), "expected an error containing {needle:?}, got {e:?}");
    }

    #[test]
    fn baseline_is_valid() {
        assert_eq!(errors(&base()), Vec::<String>::new());
    }

    #[test]
    fn rejects_unsupported_ir_version() {
        let mut v = base();
        set(&mut v, "/irVersion", json!(2));
        assert_err(&v, "unsupported irVersion");
    }

    #[test]
    fn rejects_nullable_primary_key() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/0/nullable", json!(true));
        assert_err(&v, "primary key cannot be nullable");
    }

    #[test]
    fn rejects_table_without_primary_key() {
        let mut v = base();
        set(&mut v, "/tables/0/primaryKey", json!([]));
        assert_err(&v, "no primary key");
    }

    #[test]
    fn rejects_primary_key_on_missing_column() {
        let mut v = base();
        set(&mut v, "/tables/0/primaryKey", json!(["ghost"]));
        assert_err(&v, "primary key column \"ghost\" missing");
    }

    #[test]
    fn rejects_auto_increment_on_non_integer() {
        let mut v = base();
        set(&mut v, "/tables/1/columns/1/autoIncrement", json!(true));
        assert_err(&v, "autoIncrement needs an integer/bigint primary key");
    }

    #[test]
    fn rejects_auto_increment_on_non_key_integer() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/2/autoIncrement", json!(true));
        assert_err(&v, "autoIncrement needs an integer/bigint primary key");
    }

    #[test]
    fn rejects_max_length_on_non_text() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/2/maxLength", json!(5));
        assert_err(&v, "maxLength only applies to text");
    }

    #[test]
    fn rejects_undeclared_enum() {
        let mut v = base();
        set(&mut v, "/enums", json!([]));
        assert_err(&v, "not declared in enums[]");
    }

    #[test]
    fn rejects_enum_default_that_is_not_a_member() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/4/default", json!({"kind": "literal", "value": "owner"}));
        assert_err(&v, "is not a member of enum");
    }

    #[test]
    fn rejects_non_string_enum_default() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/4/default", json!({"kind": "literal", "value": 3}));
        assert_err(&v, "is not a member of enum");
    }

    #[test]
    fn rejects_default_now_on_non_timestamp() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/3/default", json!({"kind": "now"}));
        assert_err(&v, "defaultNow() only applies to timestamps");
    }

    #[test]
    fn rejects_duplicate_column_names() {
        let mut v = base();
        let dup = v.pointer("/tables/0/columns/1").unwrap().clone();
        push(&mut v, "/tables/0/columns", dup);
        assert_err(&v, "duplicate column names");
    }

    #[test]
    fn rejects_index_on_missing_column() {
        let mut v = base();
        set(&mut v, "/tables/1/indexes/0/columns", json!(["nope"]));
        assert_err(&v, "references missing column \"nope\"");
    }

    #[test]
    fn rejects_unique_on_missing_column() {
        let mut v = base();
        set(&mut v, "/tables/0/uniques/0/columns", json!(["nope"]));
        assert_err(&v, "references missing column \"nope\"");
    }

    #[test]
    fn rejects_duplicate_index_names_across_tables() {
        let mut v = base();
        push(&mut v, "/tables/0/indexes", json!({"name": "dup", "columns": ["email"]}));
        push(&mut v, "/tables/1/indexes", json!({"name": "dup", "columns": ["title"]}));
        assert_err(&v, "is used twice");
    }

    #[test]
    fn rejects_foreign_key_to_missing_table() {
        let mut v = base();
        set(&mut v, "/tables/1/foreignKeys/0/refTable", json!("ghost"));
        assert_err(&v, "targets missing table \"ghost\"");
    }

    #[test]
    fn rejects_foreign_key_column_count_mismatch() {
        let mut v = base();
        set(&mut v, "/tables/1/foreignKeys/0/refColumns", json!(["id", "id"]));
        assert_err(&v, "column count mismatch");
    }

    #[test]
    fn rejects_foreign_key_to_missing_column() {
        let mut v = base();
        set(&mut v, "/tables/1/foreignKeys/0/refColumns", json!(["ghost"]));
        assert_err(&v, "references missing column");
    }

    #[test]
    fn rejects_foreign_key_type_mismatch() {
        let mut v = base();
        set(&mut v, "/tables/1/columns/2/type", json!({"kind": "text"}));
        assert_err(&v, "type mismatch");
    }

    #[test]
    fn rejects_foreign_key_to_non_unique_column() {
        let mut v = base();
        set(&mut v, "/tables/1/foreignKeys/0/refColumns", json!(["age"]));
        assert_err(&v, "is not unique");
    }

    #[test]
    fn accepts_foreign_key_to_unique_non_primary_column() {
        let mut v = base();
        set(&mut v, "/tables/1/columns/2/type", json!({"kind": "text"}));
        set(&mut v, "/tables/1/foreignKeys/0/refColumns", json!(["email"]));
        assert_eq!(errors(&v), Vec::<String>::new());
    }

    #[test]
    fn reports_every_error_not_just_the_first() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/2/maxLength", json!(5));
        set(&mut v, "/tables/1/indexes/0/columns", json!(["nope"]));
        assert!(errors(&v).len() >= 2, "{:?}", errors(&v));
    }

    #[test]
    fn unknown_fields_fail_to_parse() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/0/bogus", json!(1));
        assert!(serde_json::from_value::<Schema>(v).is_err());
    }

    #[test]
    fn unknown_column_type_fails_to_parse() {
        let mut v = base();
        set(&mut v, "/tables/0/columns/0/type", json!({"kind": "decimal"}));
        assert!(serde_json::from_value::<Schema>(v).is_err());
    }
}
