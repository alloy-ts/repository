//! Registration-order routers.
//!
//! Overlapping patterns do not compete by specificity. Every match runs in the
//! order it was registered. `RegExpRouter` refuses a parameter pattern when that
//! segment already has a different expression, when the expression matches the
//! empty string, or when it contains a capturing group. `TrieRouter` still
//! matches those patterns. `SmartRouter` uses the regexp router until an add is
//! rejected, then the trie matches every route, including ones registered
//! before and after the fallback.

use std::collections::HashMap;

use regex::Regex;

#[derive(Debug)]
pub struct UnsupportedPath {
    pub path: String,
}

impl std::fmt::Display for UnsupportedPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "unsupported route pattern: {}", self.path)
    }
}

#[derive(Clone, Debug)]
pub struct Matched<T> {
    pub handler: T,
    pub params: HashMap<String, String>,
    /// `ALL` for middleware and `all()`, otherwise the registered method.
    pub method: String,
}

pub trait Router<T> {
    fn name(&self) -> &'static str;
    fn add(&mut self, method: &str, path: &str, handler: T) -> Result<(), UnsupportedPath>;
    fn match_route(&self, method: &str, path: &str) -> Vec<Matched<T>>;
}

#[derive(Clone)]
struct Compiled<T> {
    method: String,
    expression: Regex,
    keys: Vec<String>,
    handler: T,
}

pub struct RegExpRouter<T> {
    routes: Vec<Compiled<T>>,
    validators: HashMap<String, ValidatorNode>,
}

impl<T> Default for RegExpRouter<T> {
    fn default() -> Self {
        Self {
            routes: Vec::new(),
            validators: HashMap::new(),
        }
    }
}

impl<T: Clone> Router<T> for RegExpRouter<T> {
    fn name(&self) -> &'static str {
        "RegExpRouter"
    }

    fn add(&mut self, method: &str, path: &str, handler: T) -> Result<(), UnsupportedPath> {
        let verb = method.to_ascii_uppercase();
        for pattern in expand_optional(path) {
            let validator = self.validators.entry(verb.clone()).or_default();
            validator.insert(&pattern)?;
            let compiled = compile(&pattern).map_err(|_| UnsupportedPath { path: path.to_owned() })?;
            self.routes.push(Compiled {
                method: verb.clone(),
                expression: compiled.expression,
                keys: compiled.keys,
                handler: handler.clone(),
            });
        }
        Ok(())
    }

    fn match_route(&self, method: &str, path: &str) -> Vec<Matched<T>> {
        let verb = method.to_ascii_uppercase();
        let mut matched = Vec::new();
        for route in &self.routes {
            if route.method != verb && route.method != "ALL" {
                continue;
            }
            let Some(captures) = route.expression.captures(path) else {
                continue;
            };
            let mut params = HashMap::new();
            for (index, key) in route.keys.iter().enumerate() {
                if let Some(value) = captures.get(index + 1) {
                    params.insert(key.clone(), try_decode(value.as_str()));
                }
            }
            matched.push(Matched {
                handler: route.handler.clone(),
                params,
                method: route.method.clone(),
            });
        }
        matched
    }
}

struct TrieHandler<T> {
    method: String,
    handler: T,
    index: usize,
}

struct ParamEdge<T> {
    name: String,
    expression: Regex,
    spans: bool,
    node: TrieNode<T>,
}

struct TrieNode<T> {
    statics: HashMap<String, TrieNode<T>>,
    params: Vec<ParamEdge<T>>,
    wild: Option<Box<TrieNode<T>>>,
    rest: Option<Box<TrieNode<T>>>,
    handlers: Vec<TrieHandler<T>>,
}

impl<T> Default for TrieNode<T> {
    fn default() -> Self {
        Self {
            statics: HashMap::new(),
            params: Vec::new(),
            wild: None,
            rest: None,
            handlers: Vec::new(),
        }
    }
}

pub struct TrieRouter<T> {
    root: TrieNode<T>,
    index: usize,
}

impl<T> Default for TrieRouter<T> {
    fn default() -> Self {
        Self {
            root: TrieNode::default(),
            index: 0,
        }
    }
}

impl<T: Clone> Router<T> for TrieRouter<T> {
    fn name(&self) -> &'static str {
        "TrieRouter"
    }

    fn add(&mut self, method: &str, path: &str, handler: T) -> Result<(), UnsupportedPath> {
        let verb = method.to_ascii_uppercase();
        for pattern in expand_optional(path) {
            let node = insert_trie(&mut self.root, &pattern);
            node.handlers.push(TrieHandler {
                method: verb.clone(),
                handler: handler.clone(),
                index: self.index,
            });
            self.index += 1;
        }
        Ok(())
    }

    fn match_route(&self, method: &str, path: &str) -> Vec<Matched<T>> {
        let verb = method.to_ascii_uppercase();
        let mut found = Vec::new();
        walk_trie(&self.root, &split_path(path), 0, &HashMap::new(), &verb, &mut found);
        found.sort_by_key(|item| item.0);
        found
            .into_iter()
            .map(|(_, handler, params, method)| Matched { handler, params, method })
            .collect()
    }
}

pub struct SmartRouter<T> {
    routers: Vec<Box<dyn Router<T> + Send>>,
}

impl<T: Clone + Send + 'static> Default for SmartRouter<T> {
    fn default() -> Self {
        Self {
            routers: vec![
                Box::new(RegExpRouter::<T>::default()),
                Box::new(TrieRouter::<T>::default()),
            ],
        }
    }
}

impl<T: Clone + Send + 'static> SmartRouter<T> {
    pub fn active(&self) -> &'static str {
        self.routers.first().map(|router| router.name()).unwrap_or("none")
    }
}

impl<T: Clone + Send + 'static> Router<T> for SmartRouter<T> {
    fn name(&self) -> &'static str {
        "SmartRouter"
    }

    fn add(&mut self, method: &str, path: &str, handler: T) -> Result<(), UnsupportedPath> {
        let mut kept = Vec::new();
        let mut added = false;
        for mut router in self.routers.drain(..) {
            match router.add(method, path, handler.clone()) {
                Ok(()) => {
                    kept.push(router);
                    added = true;
                }
                Err(_) => {}
            }
        }
        if !added {
            return Err(UnsupportedPath { path: path.to_owned() });
        }
        self.routers = kept;
        Ok(())
    }

    fn match_route(&self, method: &str, path: &str) -> Vec<Matched<T>> {
        self.routers
            .first()
            .map(|router| router.match_route(method, path))
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct ValidatorNode {
    statics: HashMap<String, ValidatorNode>,
    param: Option<(String, Box<ValidatorNode>)>,
    wild: Option<Box<ValidatorNode>>,
    rest: Option<Box<ValidatorNode>>,
}

impl ValidatorNode {
    fn insert(&mut self, pattern: &str) -> Result<(), UnsupportedPath> {
        descend(self, pattern, &route_segments(pattern))
    }
}

fn descend(node: &mut ValidatorNode, pattern: &str, parts: &[Segment]) -> Result<(), UnsupportedPath> {
    if parts.is_empty() {
        return Ok(());
    }
    let part = &parts[0];
    let rest = &parts[1..];
    match part {
        Segment::Rest => {
            let child = node.rest.get_or_insert_with(|| Box::new(ValidatorNode::default()));
            descend(child, pattern, rest)
        }
        Segment::Wild => {
            let child = node.wild.get_or_insert_with(|| Box::new(ValidatorNode::default()));
            descend(child, pattern, rest)
        }
        Segment::Static(value) => {
            let child = node.statics.entry(value.clone()).or_default();
            descend(child, pattern, rest)
        }
        Segment::Param { source, .. } => {
            if let Some((existing, _)) = &node.param {
                if existing != source {
                    return Err(UnsupportedPath { path: pattern.to_owned() });
                }
            } else if matches_empty(source) || has_capturing_group(source) {
                return Err(UnsupportedPath { path: pattern.to_owned() });
            }
            if node.param.is_none() {
                node.param = Some((source.clone(), Box::new(ValidatorNode::default())));
            }
            let child = &mut node.param.as_mut().unwrap().1;
            descend(child, pattern, rest)
        }
    }
}

enum Segment {
    Static(String),
    Param { name: String, source: String, spans: bool },
    Wild,
    Rest,
}

fn insert_trie<'a, T>(root: &'a mut TrieNode<T>, pattern: &str) -> &'a mut TrieNode<T> {
    let segments = route_segments(pattern);
    let mut node = root;
    for part in segments {
        match part {
            Segment::Rest => {
                if node.rest.is_none() {
                    node.rest = Some(Box::new(TrieNode::default()));
                }
                node = node.rest.as_mut().unwrap();
            }
            Segment::Wild => {
                if node.wild.is_none() {
                    node.wild = Some(Box::new(TrieNode::default()));
                }
                node = node.wild.as_mut().unwrap();
            }
            Segment::Static(value) => {
                node = node.statics.entry(value).or_default();
            }
            Segment::Param { name, source, spans } => {
                if let Some(index) = node
                    .params
                    .iter()
                    .position(|edge| edge.expression.as_str() == anchored(&source) && edge.name == name)
                {
                    node = &mut node.params[index].node;
                } else {
                    node.params.push(ParamEdge {
                        name,
                        expression: Regex::new(&anchored(&source)).unwrap_or_else(|_| Regex::new("$^").unwrap()),
                        spans,
                        node: TrieNode::default(),
                    });
                    let last = node.params.len() - 1;
                    node = &mut node.params[last].node;
                }
            }
        }
    }
    node
}

fn walk_trie<T: Clone>(
    node: &TrieNode<T>,
    segments: &[String],
    index: usize,
    params: &HashMap<String, String>,
    method: &str,
    found: &mut Vec<(usize, T, HashMap<String, String>, String)>,
) {
    let collect = |handlers: &[TrieHandler<T>], captured: &HashMap<String, String>, found: &mut Vec<_>| {
        for handler in handlers {
            if handler.method != "ALL" && handler.method != method {
                continue;
            }
            found.push((
                handler.index,
                handler.handler.clone(),
                captured.clone(),
                handler.method.clone(),
            ));
        }
    };
    if let Some(rest) = &node.rest {
        collect(&rest.handlers, params, found);
    }
    if index == segments.len() {
        collect(&node.handlers, params, found);
        return;
    }
    let segment = &segments[index];
    if let Some(child) = node.statics.get(segment) {
        walk_trie(child, segments, index + 1, params, method, found);
    }
    if let Some(wild) = &node.wild {
        walk_trie(wild, segments, index + 1, params, method, found);
    }
    for edge in &node.params {
        if edge.spans {
            let leaf = !edge.node.handlers.is_empty()
                && edge.node.statics.is_empty()
                && edge.node.params.is_empty()
                && edge.node.wild.is_none()
                && edge.node.rest.is_none();
            if leaf {
                let rest = segments[index..].join("/");
                if edge.expression.is_match(&rest) {
                    let mut captured = params.clone();
                    captured.insert(edge.name.clone(), try_decode(&rest));
                    walk_trie(&edge.node, segments, segments.len(), &captured, method, found);
                }
                continue;
            }
            for take in 1..=(segments.len() - index) {
                let chunk = segments[index..index + take].join("/");
                if !edge.expression.is_match(&chunk) {
                    continue;
                }
                let mut captured = params.clone();
                captured.insert(edge.name.clone(), try_decode(&chunk));
                walk_trie(&edge.node, segments, index + take, &captured, method, found);
            }
        } else if edge.expression.is_match(segment) {
            let mut captured = params.clone();
            captured.insert(edge.name.clone(), try_decode(segment));
            walk_trie(&edge.node, segments, index + 1, &captured, method, found);
        }
    }
}

fn route_segments(pattern: &str) -> Vec<Segment> {
    if pattern == "*" || pattern == "/*" {
        return vec![Segment::Rest];
    }
    let (marked, groups) = mask_groups(pattern);
    let mut rest = false;
    let mut marked = marked;
    if marked.ends_with("/*") {
        rest = true;
        marked.truncate(marked.len() - 2);
    }
    let parts: Vec<String> = marked
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| restore(part, &groups))
        .collect();
    let count = parts.len();
    let mut segments: Vec<Segment> = parts.into_iter().map(|part| classify_segment(&part, count)).collect();
    if rest {
        segments.push(Segment::Rest);
    }
    segments
}

fn classify_segment(part: &str, count: usize) -> Segment {
    if part == "/*" || (part == "*" && count == 1) {
        return Segment::Rest;
    }
    if part == "*" {
        return Segment::Wild;
    }
    if let Some(rest) = part.strip_prefix(':') {
        let (name, source) = if let Some(start) = rest.find('{') {
            let name = &rest[..start];
            let source = rest[start + 1..].trim_end_matches('}');
            (name.to_owned(), source.to_owned())
        } else {
            (rest.trim_end_matches('?').to_owned(), "[^/]+".to_owned())
        };
        let spans = spans_slash(&source);
        return Segment::Param { name, source, spans };
    }
    Segment::Static(part.to_owned())
}

fn mask_groups(pattern: &str) -> (String, Vec<String>) {
    let mut groups = Vec::new();
    let mut output = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '{' {
            let mut depth = 1;
            let start = index;
            index += 1;
            while index < chars.len() && depth > 0 {
                if chars[index] == '{' {
                    depth += 1;
                } else if chars[index] == '}' {
                    depth -= 1;
                }
                index += 1;
            }
            let group: String = chars[start..index].iter().collect();
            output.push('\u{E000}');
            output.push_str(&groups.len().to_string());
            output.push('\u{E000}');
            groups.push(group);
        } else {
            output.push(chars[index]);
            index += 1;
        }
    }
    (output, groups)
}

fn restore(value: &str, groups: &[String]) -> String {
    let mut output = String::new();
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\u{E000}' {
            let end = chars[index + 1..]
                .iter()
                .position(|char| *char == '\u{E000}')
                .map(|position| index + 1 + position);
            if let Some(end) = end {
                let number: String = chars[index + 1..end].iter().collect();
                if let Ok(group) = number.parse::<usize>() {
                    if let Some(value) = groups.get(group) {
                        output.push_str(value);
                    }
                }
                index = end + 1;
                continue;
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

struct CompiledPattern {
    expression: Regex,
    keys: Vec<String>,
}

fn compile(pattern: &str) -> Result<CompiledPattern, ()> {
    if pattern == "*" || pattern == "/*" {
        return Ok(CompiledPattern {
            expression: Regex::new(r"\A[\s\S]*\z").unwrap(),
            keys: Vec::new(),
        });
    }
    let mut keys = Vec::new();
    let mut source = String::new();
    for (index, token) in tokenize(pattern)?.into_iter().enumerate() {
        match token {
            Token::Literal(value) => source.push_str(&regex::escape(&value)),
            Token::Param { name, pattern } => {
                keys.push(name);
                source.push('(');
                source.push_str(&pattern);
                source.push(')');
            }
            Token::Star => {
                source.push_str(if index + 1 == tokenize_len(pattern) {
                    r"[\s\S]*"
                } else {
                    "[^/]+"
                });
            }
            Token::Tail => source.push_str(r"(?:|/[\s\S]*)"),
        }
    }
    Regex::new(&format!(r"\A{source}\z")).map(|expression| CompiledPattern { expression, keys }).map_err(|_| ())
}

fn tokenize_len(pattern: &str) -> usize {
    tokenize(pattern).map(|tokens| tokens.len()).unwrap_or(0)
}

enum Token {
    Literal(String),
    Param { name: String, pattern: String },
    Star,
    Tail,
}

fn tokenize(pattern: &str) -> Result<Vec<Token>, ()> {
    let (marked, groups) = mask_groups(pattern);
    let chars: Vec<char> = marked.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index..].starts_with(&['/', '*']) && index + 2 == chars.len() {
            tokens.push(Token::Tail);
            break;
        }
        let current = chars[index];
        if current == '*' {
            tokens.push(Token::Star);
            index += 1;
            continue;
        }
        if current == ':' {
            let mut end = index + 1;
            while end < chars.len() && chars[end] != '/' {
                end += 1;
            }
            let raw: String = chars[index..end].iter().collect();
            let raw = restore(&raw, &groups);
            let raw = raw.trim_end_matches('?');
            let Some(name_and_pattern) = raw.strip_prefix(':') else {
                return Err(());
            };
            let (name, pattern) = if let Some(brace) = name_and_pattern.find('{') {
                (
                    name_and_pattern[..brace].to_owned(),
                    name_and_pattern[brace + 1..].trim_end_matches('}').to_owned(),
                )
            } else {
                (name_and_pattern.to_owned(), "[^/]+".to_owned())
            };
            if name.is_empty() {
                return Err(());
            }
            tokens.push(Token::Param { name, pattern });
            index = end;
            continue;
        }
        let mut end = index;
        while end < chars.len() {
            if chars[end..].starts_with(&['/', '*']) && end + 2 == chars.len() {
                break;
            }
            if chars[end] == '*' || chars[end] == ':' {
                break;
            }
            end += 1;
        }
        let literal: String = chars[index..end].iter().collect();
        tokens.push(Token::Literal(restore(&literal, &groups)));
        index = end;
    }
    Ok(tokens)
}

fn expand_optional(path: &str) -> Vec<String> {
    if !path.ends_with('?') || !path.contains(':') {
        return vec![path.to_owned()];
    }
    let mut results = Vec::new();
    let mut base = String::new();
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        if let Some(name) = segment.strip_prefix(':') {
            if name.ends_with('?') {
                results.push(if base.is_empty() { "/".to_owned() } else { base.clone() });
                base.push('/');
                base.push_str(&segment[..segment.len() - 1]);
                results.push(base.clone());
            } else {
                base.push('/');
                base.push_str(segment);
            }
        } else {
            base.push('/');
            base.push_str(segment);
        }
    }
    if results.is_empty() {
        vec![path.to_owned()]
    } else {
        results
    }
}

fn split_path(path: &str) -> Vec<String> {
    let bare = path.split(['?', '#']).next().unwrap_or(path);
    bare.split('/').filter(|part| !part.is_empty()).map(str::to_owned).collect()
}

fn anchored(source: &str) -> String {
    format!(r"\A(?:{source})\z")
}

fn matches_empty(source: &str) -> bool {
    Regex::new(&anchored(source)).map(|expression| expression.is_match("")).unwrap_or(true)
}

fn has_capturing_group(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index] == b'(' && bytes.get(index + 1) != Some(&b'?') {
            return true;
        }
        index += 1;
    }
    false
}

fn spans_slash(source: &str) -> bool {
    let Ok(full) = Regex::new(&anchored(source)) else {
        return false;
    };
    full.is_match("/")
        || full.is_match("a/b")
        || full.is_match("a/b.png")
        || Regex::new(source).map(|expression| expression.is_match("/")).unwrap_or(false)
}

pub(crate) fn try_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_host_path_matches() {
        let mut router = RegExpRouter::default();
        router.add("GET", "/www1.example.com/hello", "hello").unwrap();
        assert_eq!(router.match_route("GET", "/www1.example.com/hello").len(), 1);
    }

    #[test]
    fn root_path_matches() {
        let mut router = RegExpRouter::default();
        router.add("GET", "/", "root").unwrap();
        assert_eq!(router.match_route("GET", "/").len(), 1);
    }

    #[test]
    fn regexp_matches_in_order_and_rejects_conflicts() {
        let mut router = RegExpRouter::default();
        router.add("GET", "/book/a", "a").unwrap();
        router.add("GET", "/book/:slug", "common").unwrap();
        router.add("ALL", "*", "any").unwrap();
        let book = router.match_route("GET", "/book/a");
        assert_eq!(book.iter().map(|route| route.handler).collect::<Vec<_>>(), ["a", "common", "any"]);
        assert_eq!(book[1].params.get("slug").map(String::as_str), Some("a"));
        assert_eq!(
            router.match_route("POST", "/book/a").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["any"]
        );
        router.add("GET", "/post/:id{[0-9]+}", "num").unwrap();
        assert!(router.add("GET", "/post/:name{[a-z]+}", "word").is_err());
        let mut empty = RegExpRouter::default();
        assert!(empty.add("GET", "/:id{.*}", "empty").is_err());
    }

    #[test]
    fn trie_keeps_conflicting_patterns_in_registration_order() {
        let mut router = TrieRouter::default();
        router.add("GET", "/post/:id{[0-9]+}", "num").unwrap();
        router.add("GET", "/post/:name{[a-z]+}", "word").unwrap();
        router.add("GET", "/post/:id{.+}", "any").unwrap();
        assert_eq!(
            router.match_route("GET", "/post/12").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["num", "any"]
        );
        assert_eq!(router.match_route("GET", "/post/12")[0].params.get("id").map(String::as_str), Some("12"));
        assert_eq!(
            router.match_route("GET", "/post/ab").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["word", "any"]
        );
        assert!(router.match_route("GET", "/files/images/cat.png").is_empty());
        router.add("GET", "/files/:filename{.+\\.png}", "file").unwrap();
        assert_eq!(
            router.match_route("GET", "/files/images/cat.png")[0].params.get("filename").map(String::as_str),
            Some("images/cat.png")
        );
    }

    #[test]
    fn smart_router_fallback_keeps_registration_order() {
        let mut router = SmartRouter::default();
        router.add("GET", "/wild/*/card", "wild").unwrap();
        router.add("GET", "/api/animal/:type?", "animal").unwrap();
        router.add("GET", "*", "star").unwrap();
        assert_eq!(router.active(), "RegExpRouter");
        assert_eq!(
            router.match_route("GET", "/wild/blue/card").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["wild", "star"]
        );
        assert_eq!(
            router.match_route("GET", "/api/animal").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["animal", "star"]
        );
        assert_eq!(
            router.match_route("GET", "/api/animal/cat")[0].params.get("type").map(String::as_str),
            Some("cat")
        );
        router.add("GET", "/post/:id{[0-9]+}", "before").unwrap();
        router.add("GET", "/post/:name{[a-z]+}", "after").unwrap();
        assert_eq!(router.active(), "TrieRouter");
        assert_eq!(
            router.match_route("GET", "/wild/blue/card").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["wild", "star"]
        );
        assert_eq!(
            router.match_route("GET", "/post/12").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["star", "before"]
        );
        assert_eq!(
            router.match_route("GET", "/post/ab").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["star", "after"]
        );
        router.add("GET", "/wild/blue/card", "later").unwrap();
        assert_eq!(
            router.match_route("GET", "/wild/blue/card").iter().map(|route| route.handler).collect::<Vec<_>>(),
            ["wild", "star", "later"]
        );
    }
}
