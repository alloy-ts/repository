use crate::engine::errors::EngineError;
use crate::engine::token::{Token, TokenKind};

pub fn tokenize(source: &str) -> Result<Vec<Token>, EngineError> {
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

        // Single-line comment // ...
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        let token_line = line;
        let token_col = col;

        // String literal "..."
        if c == '"' {
            i += 1;
            col += 1;
            let mut s = String::new();
            let mut terminated = false;

            while i < chars.len() {
                let sc = chars[i];
                if sc == '"' {
                    terminated = true;
                    i += 1;
                    col += 1;
                    break;
                } else if sc == '\\' && i + 1 < chars.len() {
                    i += 1;
                    col += 1;
                    let escape_c = chars[i];
                    match escape_c {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        other => s.push(other),
                    }
                    i += 1;
                    col += 1;
                } else if sc == '\n' {
                    s.push('\n');
                    line += 1;
                    col = 1;
                    i += 1;
                } else {
                    s.push(sc);
                    i += 1;
                    col += 1;
                }
            }

            if !terminated {
                return Err(EngineError::lex(
                    "Unterminated string literal",
                    token_line,
                    token_col,
                ));
            }

            tokens.push(Token::new(TokenKind::StringLit(s), token_line, token_col));
            continue;
        }

        // Numbers (integers and floats)
        if c.is_ascii_digit() {
            let start = i;
            let mut is_float = false;

            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || (chars[i] == '.'
                        && !is_float
                        && i + 1 < chars.len()
                        && chars[i + 1].is_ascii_digit()))
            {
                if chars[i] == '.' {
                    is_float = true;
                }
                col += 1;
                i += 1;
            }

            let num_str: String = chars[start..i].iter().collect();
            if is_float {
                let val = num_str.parse::<f64>().map_err(|e| {
                    EngineError::lex(format!("Invalid float: {e}"), token_line, token_col)
                })?;
                tokens.push(Token::new(
                    TokenKind::FloatNumber(val),
                    token_line,
                    token_col,
                ));
            } else {
                let val = num_str.parse::<i64>().map_err(|e| {
                    EngineError::lex(format!("Invalid integer: {e}"), token_line, token_col)
                })?;
                tokens.push(Token::new(TokenKind::IntNumber(val), token_line, token_col));
            }
            continue;
        }

        // Identifiers and keywords
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                col += 1;
                i += 1;
            }

            let word: String = chars[start..i].iter().collect();
            let kind = match word.as_str() {
                "let" => TokenKind::Let,
                "print" => TokenKind::Print,
                "fn" => TokenKind::Fn,
                "return" => TokenKind::Return,
                "if" => TokenKind::If,
                "else" => TokenKind::Else,
                "while" => TokenKind::While,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Identifier(word),
            };

            tokens.push(Token::new(kind, token_line, token_col));
            continue;
        }

        // Two-character operators or single-character
        let next_c = if i + 1 < chars.len() {
            Some(chars[i + 1])
        } else {
            None
        };

        let (kind, consumed) = match (c, next_c) {
            ('=', Some('=')) => (TokenKind::EqualEqual, 2),
            ('!', Some('=')) => (TokenKind::BangEqual, 2),
            ('<', Some('=')) => (TokenKind::LessEqual, 2),
            ('>', Some('=')) => (TokenKind::GreaterEqual, 2),
            ('=', _) => (TokenKind::Equal, 1),
            ('+', _) => (TokenKind::Plus, 1),
            ('-', _) => (TokenKind::Minus, 1),
            ('*', _) => (TokenKind::Star, 1),
            ('/', _) => (TokenKind::Slash, 1),
            ('<', _) => (TokenKind::Less, 1),
            ('>', _) => (TokenKind::Greater, 1),
            ('!', _) => (TokenKind::Bang, 1),
            ('(', _) => (TokenKind::LeftParen, 1),
            (')', _) => (TokenKind::RightParen, 1),
            ('{', _) => (TokenKind::LeftBrace, 1),
            ('}', _) => (TokenKind::RightBrace, 1),
            (',', _) => (TokenKind::Comma, 1),
            (';', _) => (TokenKind::Semicolon, 1),
            _ => {
                return Err(EngineError::lex(
                    format!("Unexpected character: '{c}'"),
                    token_line,
                    token_col,
                ));
            }
        };

        tokens.push(Token::new(kind, token_line, token_col));
        i += consumed;
        col += consumed;
    }

    tokens.push(Token::new(TokenKind::Eof, line, col));
    Ok(tokens)
}
