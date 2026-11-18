use regex::Regex;
use std::collections::HashMap;

pub struct Route {
    pub method: String,
    pub handler_id: u32,
    pub is_middleware: bool,
    pub error_boundary_id: Option<u32>,
    regex: Regex,
    names: Vec<String>,
}
#[derive(Debug, PartialEq)]
pub struct Match {
    pub handler_id: u32,
    pub params: HashMap<String, String>,
    pub is_middleware: bool,
    pub error_boundary_id: Option<u32>,
}
impl Route {
    pub fn new(
        method: String,
        pattern: &str,
        handler_id: u32,
        is_middleware: bool,
        error_boundary_id: Option<u32>,
        strict: bool,
    ) -> Result<Self, String> {
        if pattern != "*" && !pattern.starts_with('/') {
            return Err("route pattern must start with /".into());
        }
        let mut source = String::from("^");
        let mut names = Vec::new();
        if pattern == "*" {
            source.push_str(".*");
        } else {
            let mut segment = String::new();
            let mut depth = 0usize;
            let mut segments = Vec::new();
            for ch in pattern[1..].chars() {
                if ch == '/' && depth == 0 {
                    segments.push(std::mem::take(&mut segment));
                } else {
                    if ch == '{' {
                        depth += 1;
                    }
                    if ch == '}' {
                        depth = depth.checked_sub(1).ok_or("unbalanced route regex")?;
                    }
                    segment.push(ch);
                }
            }
            if depth != 0 {
                return Err("unbalanced route regex".into());
            }
            segments.push(segment);
            for part in segments {
                if part == "*" {
                    source.push_str("/.*");
                    continue;
                }
                if let Some(param) = part.strip_prefix(':') {
                    let optional = param.ends_with('?');
                    let param = param.strip_suffix('?').unwrap_or(param);
                    let (name, constraint) = if let Some(open) = param.find('{') {
                        if !param.ends_with('}') {
                            return Err("invalid parameter constraint".into());
                        }
                        (&param[..open], &param[open + 1..param.len() - 1])
                    } else {
                        (param, "[^/]+")
                    };
                    if name.is_empty()
                        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return Err("invalid parameter name".into());
                    }
                    let capture = format!("(?P<p{}>{constraint})", names.len());
                    if optional {
                        source.push_str(&format!("(?:/{capture})?"));
                    } else {
                        source.push('/');
                        source.push_str(&capture);
                    }
                    names.push(name.to_owned());
                } else {
                    source.push('/');
                    source.push_str(&regex::escape(&part));
                }
            }
        }
        if !strict && pattern != "/" && pattern != "*" {
            source.push_str("/?");
        }
        source.push('$');
        let regex = Regex::new(&source).map_err(|e| e.to_string())?;
        Ok(Self {
            method: method.to_ascii_uppercase(),
            handler_id,
            is_middleware,
            error_boundary_id,
            regex,
            names,
        })
    }
    pub fn matches(&self, method: &str, path: &str) -> Option<Match> {
        if self.method != "*" && self.method != method.to_ascii_uppercase() {
            return None;
        }
        let captures = self.regex.captures(path)?;
        let params = self
            .names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                captures
                    .name(&format!("p{index}"))
                    .map(|value| (name.clone(), value.as_str().to_owned()))
            })
            .collect();
        Some(Match {
            handler_id: self.handler_id,
            params,
            is_middleware: self.is_middleware,
            error_boundary_id: self.error_boundary_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiles_static_optional_wildcard_and_constrained_paths() {
        let route =
            |pattern, strict| Route::new("GET".into(), pattern, 7, false, None, strict).unwrap();
        assert!(route("/hello", true).matches("GET", "/hello/").is_none());
        assert!(route("/hello", false).matches("GET", "/hello/").is_some());
        assert!(
            route("/animals/:type?", true)
                .matches("GET", "/animals")
                .unwrap()
                .params
                .is_empty()
        );
        assert_eq!(
            route("/animals/:type?", true)
                .matches("GET", "/animals/dog")
                .unwrap()
                .params["type"],
            "dog"
        );
        assert!(
            route("/wild/*/card", true)
                .matches("GET", "/wild/a/b/card")
                .is_some()
        );
        assert_eq!(
            route("/posts/:filename{.+\\.png}", true)
                .matches("GET", "/posts/a/b.png")
                .unwrap()
                .params["filename"],
            "a/b.png"
        );
        assert!(
            route("/post/:date{[0-9]+}", true)
                .matches("GET", "/post/nope")
                .is_none()
        );
        assert!(
            Route::new("*".into(), "*", 1, true, None, true)
                .unwrap()
                .matches("PURGE", "/x")
                .is_some()
        );
    }
}
