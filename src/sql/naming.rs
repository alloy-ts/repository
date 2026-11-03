/// Naming rules and casing converters for schema tables, columns, and Rust structs.

/// Convert any identifier (camelCase, PascalCase, kebab-case, snake_case) to standard `snake_case`.
pub fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev_is_lower = false;
    let mut prev_is_sep = false;

    for (i, ch) in s.chars().enumerate() {
        if ch == '_' || ch == '-' || ch == ' ' || ch == '.' {
            if !prev_is_sep && i > 0 {
                out.push('_');
                prev_is_sep = true;
            }
            prev_is_lower = false;
            continue;
        }

        if ch.is_uppercase() {
            if prev_is_lower && !prev_is_sep {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
            prev_is_lower = false;
            prev_is_sep = false;
        } else {
            out.push(ch);
            prev_is_lower = true;
            prev_is_sep = false;
        }
    }

    out.trim_matches('_').to_string()
}

/// Convert any identifier to `PascalCase`.
pub fn to_pascal_case(s: &str) -> String {
    let snake = to_snake_case(s);
    let mut out = String::with_capacity(snake.len());
    let mut cap_next = true;

    for ch in snake.chars() {
        if ch == '_' {
            cap_next = true;
        } else if cap_next {
            out.extend(ch.to_uppercase());
            cap_next = false;
        } else {
            out.push(ch);
        }
    }

    out
}

/// Singularize a common English table name (e.g. "users" -> "user", "categories" -> "category", "posts" -> "post").
pub fn singularize(word: &str) -> String {
    let lower = word.to_lowercase();
    if lower.ends_with("ies") && lower.len() > 3 {
        format!("{}y", &word[..word.len() - 3])
    } else if (lower.ends_with("ses")
        || lower.ends_with("shes")
        || lower.ends_with("ches")
        || lower.ends_with("xes")
        || lower.ends_with("zes"))
        && word.len() > 3
    {
        word[..word.len() - 2].to_string()
    } else if lower.ends_with('s') && !lower.ends_with("ss") && lower.len() > 1 {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// Sanitize a Rust identifier to avoid Rust reserved keyword collisions (e.g. `type` -> `r#type`).
pub fn sanitize_rust_ident(name: &str) -> String {
    match name {
        "as" | "break" | "const" | "continue" | "crate" | "else" | "enum" | "extern" | "false"
        | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match" | "mod" | "move"
        | "mut" | "pub" | "ref" | "return" | "self" | "Self" | "static" | "struct" | "super"
        | "trait" | "true" | "type" | "unsafe" | "use" | "where" | "while" | "async" | "await"
        | "dyn" | "try" => format!("r#{name}"),
        _ => name.to_string(),
    }
}

/// Resolve the Rust model struct name for a table.
pub fn resolve_struct_name(table_name: &str, rust_name_override: Option<&str>) -> String {
    if let Some(r_name) = rust_name_override {
        to_pascal_case(r_name)
    } else {
        let singular = singularize(table_name);
        to_pascal_case(&singular)
    }
}

/// Resolve the Rust insert struct name for a table (e.g. `NewUser`).
pub fn resolve_new_struct_name(table_name: &str, rust_name_override: Option<&str>) -> String {
    let base = resolve_struct_name(table_name, rust_name_override);
    format!("New{base}")
}

/// Resolve the Rust field name for a column.
pub fn resolve_field_name(column_name: &str, rust_name_override: Option<&str>) -> String {
    if let Some(r_name) = rust_name_override {
        sanitize_rust_ident(&to_snake_case(r_name))
    } else {
        sanitize_rust_ident(&to_snake_case(column_name))
    }
}

/// Resolve the Rust enum type name.
pub fn resolve_enum_type_name(enum_name: &str, rust_name_override: Option<&str>) -> String {
    if let Some(r_name) = rust_name_override {
        to_pascal_case(r_name)
    } else {
        to_pascal_case(enum_name)
    }
}

/// Resolve the Rust enum variant name.
pub fn resolve_enum_variant_name(variant: &str) -> String {
    to_pascal_case(variant)
}

/// Construct a canonical index name `{table}_{col1}_{col2}_idx`.
pub fn canonical_index_name(table_name: &str, cols: &[String]) -> String {
    let joined = cols.join("_");
    format!("{table_name}_{joined}_idx")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_casing_conversions() {
        assert_eq!(to_snake_case("createdAt"), "created_at");
        assert_eq!(to_snake_case("AuthorId"), "author_id");
        assert_eq!(to_snake_case("user_role"), "user_role");
        assert_eq!(to_snake_case("already_snake_case"), "already_snake_case");

        assert_eq!(to_pascal_case("users"), "Users");
        assert_eq!(to_pascal_case("user_role"), "UserRole");
        assert_eq!(to_pascal_case("created_at"), "CreatedAt");
    }

    #[test]
    fn test_singularization() {
        assert_eq!(singularize("users"), "user");
        assert_eq!(singularize("posts"), "post");
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("addresses"), "address");
        assert_eq!(singularize("user"), "user");
    }

    #[test]
    fn test_rust_names() {
        assert_eq!(resolve_struct_name("users", None), "User");
        assert_eq!(resolve_struct_name("users", Some("Account")), "Account");
        assert_eq!(resolve_new_struct_name("users", None), "NewUser");
        assert_eq!(resolve_field_name("type", None), "r#type");
        assert_eq!(resolve_field_name("authorId", None), "author_id");
    }
}
