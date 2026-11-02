//! Lexer for the Engine programming language.

use crate::engine::token::{Span, SpannedToken, Token};

pub fn tokenize(source: &str) -> Result<Vec<SpannedToken>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut col = 1;

    while i < chars.len() {
        let c = chars[i];

        if c == '\n' {
            line += 1;
            col = 1;
            i += 1;
            continue;
        }

        if c.is_whitespace() {
            col += 1;
            i += 1;
            continue;
        }

        // Comments: // ...
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        let start_span = Span { line, column: col };

        // Numbers: integer or float
        if c.is_ascii_digit() {
            let start = i;
            let mut is_float = false;

            while i < chars.len() && (chars[i].is_ascii_digit() || (chars[i] == '.' && !is_float && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())) {
                if chars[i] == '.' {
                    is_float = true;
                }
                col += 1;
                i += 1;
            }

            let num_str: String = chars[start..i].iter().collect();
            let token = if is_float {
                let val = num_str.parse::<f64>().map_err(|e| format!("{}: {}", e, num_str))?;
                Token::Float(val)
            } else {
                let val = num_str.parse::<i64>().map_err(|e| format!("{}: {}", e, num_str))?;
                Token::Number(val)
            };

            tokens.push(SpannedToken { token, span: start_span });
            continue;
        }

        // Strings: "..."
        if c == '"' {
            i += 1;
            col += 1;
            let mut s = String::new();
            let mut closed = false;

            while i < chars.len() {
                let sc = chars[i];
                if sc == '"' {
                    i += 1;
                    col += 1;
                    closed = true;
                    break;
                } else if sc == '\\' && i + 1 < chars.len() {
                    i += 1;
                    col += 1;
                    match chars[i] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        other => s.push(other),
                    }
                } else {
                    s.push(sc);
                }
                i += 1;
                col += 1;
            }

            if !closed {
                return Err(format!("Unterminated string literal at line {}, column {}", start_span.line, start_span.column));
            }

            tokens.push(SpannedToken { token: Token::StringLit(s), span: start_span });
            continue;
        }

        // Identifiers and keywords
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;

            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
                col += 1;
            }

            let word: String = chars[start..i].iter().collect();
            let token = match word.as_str() {
                "let" => Token::Let,
                "print" => Token::Print,
                "if" => Token::If,
                "else" => Token::Else,
                "while" => Token::While,
                "fn" => Token::Fn,
                "return" => Token::Return,
                "true" => Token::Boolean(true),
                "false" => Token::Boolean(false),
                _ => Token::Identifier(word),
            };

            tokens.push(SpannedToken { token, span: start_span });
            continue;
        }

        // Multi-character operators
        let token = match c {
            '+' => {
                i += 1;
                col += 1;
                Token::Plus
            }
            '-' => {
                i += 1;
                col += 1;
                Token::Minus
            }
            '*' => {
                i += 1;
                col += 1;
                Token::Star
            }
            '/' => {
                i += 1;
                col += 1;
                Token::Slash
            }
            '(' => {
                i += 1;
                col += 1;
                Token::LeftParen
            }
            ')' => {
                i += 1;
                col += 1;
                Token::RightParen
            }
            '{' => {
                i += 1;
                col += 1;
                Token::LeftBrace
            }
            '}' => {
                i += 1;
                col += 1;
                Token::RightBrace
            }
            ',' => {
                i += 1;
                col += 1;
                Token::Comma
            }
            ';' => {
                i += 1;
                col += 1;
                Token::Semicolon
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    col += 2;
                    Token::EqualEqual
                } else {
                    i += 1;
                    col += 1;
                    Token::Equal
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    col += 2;
                    Token::BangEqual
                } else {
                    i += 1;
                    col += 1;
                    Token::Bang
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    col += 2;
                    Token::LessEqual
                } else {
                    i += 1;
                    col += 1;
                    Token::Less
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    col += 2;
                    Token::GreaterEqual
                } else {
                    i += 1;
                    col += 1;
                    Token::Greater
                }
            }
            _ => return Err(format!("Unexpected character '{}' at line {}, column {}", c, line, col)),
        };

        tokens.push(SpannedToken { token, span: start_span });
    }

    tokens.push(SpannedToken {
        token: Token::Eof,
        span: Span { line, column: col },
    });

    Ok(tokens)
}
