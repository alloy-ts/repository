use serde::{Deserialize, Serialize};

/// Integer comparison filter for queries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntFilter {
    pub eq: Option<i32>,
    pub ne: Option<i32>,
    pub lt: Option<i32>,
    pub lte: Option<i32>,
    pub gt: Option<i32>,
    pub gte: Option<i32>,
    pub r#in: Option<Vec<i32>>,
    pub not_in: Option<Vec<i32>>,
    pub is_null: Option<bool>,
}

/// 64-bit integer comparison filter for bigint queries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BigIntFilter {
    pub eq: Option<i64>,
    pub ne: Option<i64>,
    pub lt: Option<i64>,
    pub lte: Option<i64>,
    pub gt: Option<i64>,
    pub gte: Option<i64>,
    pub r#in: Option<Vec<i64>>,
    pub not_in: Option<Vec<i64>>,
    pub is_null: Option<bool>,
}

/// String comparison filter with regex / pattern helpers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StringFilter {
    pub eq: Option<String>,
    pub ne: Option<String>,
    pub r#in: Option<Vec<String>>,
    pub not_in: Option<Vec<String>>,
    pub contains: Option<String>,
    pub starts_with: Option<String>,
    pub ends_with: Option<String>,
    pub is_null: Option<bool>,
}

/// Boolean filter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoolFilter {
    pub eq: Option<bool>,
    pub ne: Option<bool>,
    pub is_null: Option<bool>,
}

/// Timestamp / DateTime filter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateFilter {
    pub eq: Option<String>,
    pub ne: Option<String>,
    pub lt: Option<String>,
    pub lte: Option<String>,
    pub gt: Option<String>,
    pub gte: Option<String>,
    pub is_null: Option<bool>,
}

/// Generic ordering direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderDirection {
    Asc,
    Desc,
}

impl OrderDirection {
    pub fn as_sql(&self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}
