//! Parsing trailers from commit messages.

use itertools::Itertools as _;
use napi::bindgen_prelude::Either;
use napi_derive::napi;
use thiserror::Error;

/// A plain JavaScript object representing a trailer (for backwards compatibility).
#[derive(Clone, Debug, PartialEq, Eq)]
#[napi(object)]
pub struct TrailerObject {
    /// The trailer key/token.
    pub token: Option<String>,
    /// The trailer key.
    pub key: Option<String>,
    /// The trailer value.
    pub value: String,
}

impl TrailerObject {
    fn resolved_key(&self) -> String {
        self.key
            .as_deref()
            .or(self.token.as_deref())
            .unwrap_or_default()
            .to_string()
    }
}

/// A key-value pair representing a trailer in a commit message, of the
/// form `Key: Value`.
#[derive(Debug, PartialEq, Eq, Clone)]
#[napi]
pub struct Trailer {
    /// trailer key
    pub key: String,
    /// trailer value
    ///
    /// It is trimmed at the start and the end but includes new line characters
    /// (\n) and multi-line escape chars ( ) for multi line values.
    pub value: String,
}

#[napi]
impl Trailer {
    /// Construct a new Trailer with key and value.
    #[napi(constructor)]
    pub fn new(key: String, value: String) -> Self {
        Trailer { key, value }
    }

    /// The trailer key (token).
    #[napi(getter)]
    pub fn key(&self) -> String {
        self.key.clone()
    }

    #[napi(setter)]
    pub fn set_key(&mut self, key: String) {
        self.key = key;
    }

    /// Alias for key to maintain backward compatibility with `token`.
    #[napi(getter)]
    pub fn token(&self) -> String {
        self.key.clone()
    }

    #[napi(setter)]
    pub fn set_token(&mut self, token: String) {
        self.key = token;
    }

    /// The trailer value.
    #[napi(getter)]
    pub fn value(&self) -> String {
        self.value.clone()
    }

    #[napi(setter)]
    pub fn set_value(&mut self, value: String) {
        self.value = value;
    }

    /// Format this trailer with an optional separator (default: `": "`).
    #[napi]
    pub fn format(&self, separator: Option<String>) -> String {
        let sep = separator.unwrap_or_else(|| ": ".to_string());
        format!("{}{}{}", self.key, sep, self.value)
    }

    /// Parse the trailers from a commit message.
    #[napi]
    pub fn parse_description_trailers(body: String) -> Vec<Trailer> {
        parse_description_trailers(&body)
    }

    /// Parse the trailers from a trailer paragraph.
    #[napi]
    pub fn parse_trailers(body: String) -> napi::Result<Vec<Trailer>> {
        parse_trailers(&body).map_err(|e| napi::Error::from_reason(e.to_string()))
    }
}

#[expect(missing_docs)]
#[derive(Error, Debug)]
pub enum TrailerParseError {
    #[error("The trailer paragraph can't contain a blank line")]
    BlankLine,
    #[error("Invalid trailer line: {line}")]
    NonTrailerLine { line: String },
}

/// Parse the trailers from a commit message; these are simple key-value
/// pairs, separated by a colon, describing extra information in a commit
/// message; an example is the following:
///
/// ```text
/// chore: update itertools to version 0.14.0
///
/// Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod
/// tempor incididunt ut labore et dolore magna aliqua.
///
/// Co-authored-by: Alice <alice@example.com>
/// Co-authored-by: Bob <bob@example.com>
/// Reviewed-by: Charlie <charlie@example.com>
/// Change-Id: I1234567890abcdef1234567890abcdef12345678
/// ```
///
/// In this case, there are four trailers: two `Co-authored-by` lines, one
/// `Reviewed-by` line, and one `Change-Id` line.
pub fn parse_description_trailers(body: &str) -> Vec<Trailer> {
    let (trailers, blank, found_git_trailer, non_trailer) = parse_trailers_impl(body);
    if !blank {
        // no blank found, this means there was a single paragraph, so whatever
        // was found can't come from the trailer
        vec![]
    } else if non_trailer.is_some() && !found_git_trailer {
        // at least one non trailer line was found in the trailers paragraph
        // the trailers are considered as trailers only if there is a predefined
        // trailers from git
        vec![]
    } else {
        trailers
    }
}

/// Parse the trailers from a trailer paragraph. This function behaves like
/// `parse_description_trailer`, but will return an error if a blank or
/// non trailer line is found.
pub fn parse_trailers(body: &str) -> std::result::Result<Vec<Trailer>, TrailerParseError> {
    let (trailers, blank, _, non_trailer) = parse_trailers_impl(body);
    if blank {
        return Err(TrailerParseError::BlankLine);
    }
    if let Some(line) = non_trailer {
        return Err(TrailerParseError::NonTrailerLine { line });
    }
    Ok(trailers)
}

fn parse_trailers_impl(body: &str) -> (Vec<Trailer>, bool, bool, Option<String>) {
    // a trailer always comes at the end of a message; we can split the message
    // by newline, but we need to immediately reverse the order of the lines
    // to ensure we parse the trailer in an unambiguous manner; this avoids cases
    // where a colon in the body of the message is mistaken for a trailer
    let lines = body.trim_ascii_end().lines().rev();
    let trailer_re =
        regex::Regex::new(r"^([a-zA-Z0-9-]+) *: *(.*)$").expect("Trailer regex should be valid");
    let mut trailers: Vec<Trailer> = Vec::new();
    let mut multiline_value = vec![];
    let mut found_blank = false;
    let mut found_git_trailer = false;
    let mut non_trailer_line = None;
    for line in lines {
        if line.starts_with(' ') {
            multiline_value.push(line);
        } else if let Some(groups) = trailer_re.captures(line) {
            let key = groups[1].to_string();
            multiline_value.push(groups.get(2).unwrap().as_str());
            // trim the end of the multiline value
            // the start is already trimmed with the regex
            multiline_value[0] = multiline_value[0].trim_ascii_end();
            let value = multiline_value.iter().rev().join("\n");
            multiline_value.clear();
            if key == "Signed-off-by" {
                found_git_trailer = true;
            }
            trailers.push(Trailer { key, value });
        } else if line.starts_with("(cherry picked from commit ") {
            found_git_trailer = true;
            non_trailer_line = Some(line.to_owned());
            multiline_value.clear();
        } else if line.trim_ascii().is_empty() {
            // end of the trailer
            found_blank = true;
            break;
        } else {
            // a non trailer in the trailer paragraph
            // the line is ignored, as well as the multiline value that may
            // have previously been accumulated
            multiline_value.clear();
            non_trailer_line = Some(line.to_owned());
        }
    }
    // reverse the insert order, since we parsed the trailer in reverse
    trailers.reverse();
    (trailers, found_blank, found_git_trailer, non_trailer_line)
}

/// Parse the `git interpret-trailers` trailers of an arbitrary message, rather
/// than one attached to a commit. Everything after a `---` cut line is ignored,
/// like git does.
#[napi]
pub fn message_trailers(message: String) -> napi::Result<Vec<Trailer>> {
    let cut = cut_at_line(&message, "---");
    let parsed = gix::objs::commit::MessageRef::from_bytes(cut.as_bytes());
    Ok(match parsed.body() {
        Some(body) => body
            .trailers()
            .map(|trailer| Trailer {
                key: String::from_utf8_lossy(trailer.token).into_owned(),
                value: String::from_utf8_lossy(trailer.value.as_ref()).into_owned(),
            })
            .collect(),
        None => Vec::new(),
    })
}

/// Render a single trailer as `token<separator>value`, mirroring the pasted
/// `Trailer::display`. The default separator is `": "`.
#[napi]
pub fn format_trailer(
    trailer: Either<&Trailer, TrailerObject>,
    separator: Option<String>,
) -> String {
    let sep = separator.unwrap_or_else(|| ": ".to_string());
    match trailer {
        Either::A(t) => t.format(Some(sep)),
        Either::B(obj) => format!("{}{}{}", obj.resolved_key(), sep, obj.value),
    }
}

/// Render each trailer as `token<separator>value`, one per line (no trailing
/// newline), mirroring the pasted `Trailers::to_string`. The default separator
/// is `": "`.
#[napi]
pub fn format_trailers(
    trailers: Vec<Either<&Trailer, TrailerObject>>,
    separator: Option<String>,
) -> String {
    let sep = separator.unwrap_or_else(|| ": ".to_string());
    trailers
        .into_iter()
        .map(|t| match t {
            Either::A(tr) => format!("{}{}{}", tr.key, sep, tr.value),
            Either::B(obj) => format!("{}{}{}", obj.resolved_key(), sep, obj.value),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cut_at_line<'a>(message: &'a str, separator: &str) -> &'a str {
    let mut offset = 0;
    for chunk in message.split_inclusive('\n') {
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == separator {
            return &message[..offset];
        }
        offset += chunk.len();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_trailers() {
        let descriptions = r#"chore: update itertools to version 0.14.0

Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed
do eiusmod tempor incididunt ut labore et dolore magna aliqua.

Co-authored-by: Alice <alice@example.com>
Co-authored-by: Bob <bob@example.com>
Reviewed-by: Charlie <charlie@example.com>
Change-Id: I1234567890abcdef1234567890abcdef12345678
"#;

        let trailers = parse_description_trailers(descriptions);
        assert_eq!(trailers.len(), 4);

        assert_eq!(trailers[0].key, "Co-authored-by");
        assert_eq!(trailers[0].value, "Alice <alice@example.com>");

        assert_eq!(trailers[1].key, "Co-authored-by");
        assert_eq!(trailers[1].value, "Bob <bob@example.com>");

        assert_eq!(trailers[2].key, "Reviewed-by");
        assert_eq!(trailers[2].value, "Charlie <charlie@example.com>");

        assert_eq!(trailers[3].key, "Change-Id");
        assert_eq!(
            trailers[3].value,
            "I1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_trailers_with_colon_in_body() {
        let descriptions = r#"chore: update itertools to version 0.14.0

Summary: Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod
tempor incididunt ut labore et dolore magna aliqua.

Change-Id: I1234567890abcdef1234567890abcdef12345678
"#;

        let trailers = parse_description_trailers(descriptions);

        // should only have Change-Id
        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers[0].key, "Change-Id");
    }

    #[test]
    fn test_multiline_trailer() {
        let description = "chore: update itertools to version 0.14.0\n\nkey: This is a very long value, with spaces and\n  newlines in it.\n";

        let trailers = parse_description_trailers(description);

        // should only have key
        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers[0].key, "key");
        assert_eq!(
            trailers[0].value,
            "This is a very long value, with spaces and\n  newlines in it."
        );
    }

    #[test]
    fn test_ignore_line_in_trailer() {
        let description = r#"chore: update itertools to version 0.14.0

Signed-off-by: Random J Developer <random@developer.example.org>
[lucky@maintainer.example.org: struct foo moved from foo.c to foo.h]
Signed-off-by: Lucky K Maintainer <lucky@maintainer.example.org>
"#;

        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 2);
    }

    #[test]
    fn test_trailers_with_single_line_description() {
        let description = r#"chore: update itertools to version 0.14.0"#;
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_parse_trailers() {
        let trailers_txt = "foo: 1\nbar: 2\n";
        let res = parse_trailers(trailers_txt);
        let trailers = res.expect("trailers to be valid");
        assert_eq!(trailers.len(), 2);
        assert_eq!(trailers[0].key, "foo");
        assert_eq!(trailers[0].value, "1");
        assert_eq!(trailers[1].key, "bar");
        assert_eq!(trailers[1].value, "2");
    }

    #[test]
    fn test_blank_line_in_trailers() {
        let trailers = "foo: 1\n\nfoo: 2\n";
        let res = parse_trailers(trailers);
        assert!(matches!(res, Err(TrailerParseError::BlankLine)));
    }

    #[test]
    fn test_non_trailer_line_in_trailers() {
        let trailers = "bar\nfoo: 1\n";
        let res = parse_trailers(trailers);
        assert!(matches!(res, Err(TrailerParseError::NonTrailerLine { .. })));
    }

    #[test]
    fn test_blank_line_after_trailer() {
        let description = "subject\n\nfoo: 1\n\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
    }

    #[test]
    fn test_blank_line_inbetween() {
        let description = "subject\n\nfoo: 1\n\nbar: 2\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
    }

    #[test]
    fn test_no_blank_line() {
        let description = "subject: whatever\nfoo: 1\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_whitespace_before_key() {
        let description = "subject\n\n foo: 1\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_whitespace_after_key() {
        let description = "subject\n\nfoo : 1\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers[0].key, "foo");
    }

    #[test]
    fn test_whitespace_around_value() {
        let description = "subject\n\nfoo:  1 \n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers[0].value, "1");
    }

    #[test]
    fn test_whitespace_around_multiline_value() {
        let description = "subject\n\nfoo:  1 \n 2 \n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers[0].value, "1 \n 2");
    }

    #[test]
    fn test_whitespace_around_multiliple_trailers() {
        let description = "subject\n\nfoo:  1 \nbar:  2 \n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 2);
        assert_eq!(trailers[0].value, "1");
        assert_eq!(trailers[1].value, "2");
    }

    #[test]
    fn test_no_whitespace_before_value() {
        let description = "subject\n\nfoo:1\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
    }

    #[test]
    fn test_empty_value() {
        let description = "subject\n\nfoo:\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
    }

    #[test]
    fn test_invalid_key() {
        let description = "subject\n\nf_o_o: bar\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_content_after_trailer() {
        let description = "subject\n\nfoo: bar\nbaz\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_invalid_content_after_trailer() {
        let description = "subject\n\nfoo: bar\n\nbaz\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_empty_description() {
        let description = "";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 0);
    }

    #[test]
    fn test_cherry_pick_trailer() {
        let description = "subject\n\nsome non-trailer text\nfoo: bar\n(cherry picked from commit 72bb9f9cf4bbb6bbb11da9cda4499c55c44e87b9)\n";
        let trailers = parse_description_trailers(description);
        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers[0].key, "foo");
        assert_eq!(trailers[0].value, "bar");
    }
}
