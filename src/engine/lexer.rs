use crate::engine::errors::{EngineError, Span};
use crate::engine::token::{Token, TokenKind};

pub struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            _phantom: std::marker::PhantomData,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        if let Some(&c) = self.chars.get(self.pos) {
            self.pos += 1;
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            Some(c)
        } else {
            None
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, EngineError> {
        let mut tokens = Vec::new();

        while self.pos < self.chars.len() {
            let start_line = self.line;
            let start_col = self.col;
            let span = Span::new(start_line, start_col);

            let c = match self.peek() {
                Some(c) => c,
                None => break,
            };

            // Whitespace
            if c.is_whitespace() {
                self.advance();
                continue;
            }

            // Comments (// until end of line)
            if c == '/' && self.peek_next() == Some('/') {
                while let Some(ch) = self.peek() {
                    self.advance();
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }

            // String literal
            if c == '"' {
                let token = self.string_literal(span)?;
                tokens.push(token);
                continue;
            }

            // Numbers: int or float
            if c.is_ascii_digit() {
                let token = self.number_literal(span)?;
                tokens.push(token);
                continue;
            }

            // Identifiers / keywords
            if c.is_ascii_alphabetic() || c == '_' {
                let token = self.identifier(span);
                tokens.push(token);
                continue;
            }

            // Symbols & operators
            self.advance();
            let kind = match c {
                '+' => TokenKind::Plus,
                '-' => TokenKind::Minus,
                '*' => TokenKind::Star,
                '/' => TokenKind::Slash,
                '=' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::EqualEqual
                    } else {
                        TokenKind::Equal
                    }
                }
                '!' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::BangEqual
                    } else {
                        TokenKind::Bang
                    }
                }
                '<' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::LessEqual
                    } else {
                        TokenKind::Less
                    }
                }
                '>' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::GreaterEqual
                    } else {
                        TokenKind::Greater
                    }
                }
                '(' => TokenKind::LeftParen,
                ')' => TokenKind::RightParen,
                '{' => TokenKind::LeftBrace,
                '}' => TokenKind::RightBrace,
                ',' => TokenKind::Comma,
                ';' => TokenKind::Semicolon,
                other => {
                    return Err(EngineError::lex(
                        format!("Unexpected character: '{other}'"),
                        start_line,
                        start_col,
                    ));
                }
            };

            tokens.push(Token::new(kind, span));
        }

        tokens.push(Token::new(TokenKind::Eof, Span::new(self.line, self.col)));
        Ok(tokens)
    }

    fn string_literal(&mut self, span: Span) -> Result<Token, EngineError> {
        self.advance(); // consume opening quote '"'
        let mut text = String::new();

        while let Some(c) = self.advance() {
            match c {
                '"' => return Ok(Token::new(TokenKind::String(text), span)),
                '\\' => match self.advance() {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('r') => text.push('\r'),
                    Some('\\') => text.push('\\'),
                    Some('"') => text.push('"'),
                    Some('0') => text.push('\0'),
                    Some(other) => {
                        return Err(EngineError::lex(
                            format!("Invalid escape sequence '\\{other}'"),
                            span.line,
                            span.column,
                        ));
                    }
                    None => {
                        return Err(EngineError::lex(
                            "Unterminated escape sequence",
                            span.line,
                            span.column,
                        ));
                    }
                },
                '\n' => {
                    return Err(EngineError::lex(
                        "Unterminated string literal",
                        span.line,
                        span.column,
                    ));
                }
                other => text.push(other),
            }
        }

        Err(EngineError::lex(
            "Unterminated string literal",
            span.line,
            span.column,
        ))
    }

    fn number_literal(&mut self, span: Span) -> Result<Token, EngineError> {
        let mut s = String::new();
        let mut is_float = false;

        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.advance();
            } else if c == '.' && !is_float && self.peek_next().map_or(false, |n| n.is_ascii_digit()) {
                is_float = true;
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        if is_float {
            match s.parse::<f64>() {
                Ok(val) => Ok(Token::new(TokenKind::Float(val), span)),
                Err(e) => Err(EngineError::lex(
                    format!("Invalid float literal: {e}"),
                    span.line,
                    span.column,
                )),
            }
        } else {
            match s.parse::<i64>() {
                Ok(val) => Ok(Token::new(TokenKind::Int(val), span)),
                Err(e) => Err(EngineError::lex(
                    format!("Invalid integer literal: {e}"),
                    span.line,
                    span.column,
                )),
            }
        }
    }

    fn identifier(&mut self, span: Span) -> Token {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        let kind = match s.as_str() {
            "let" => TokenKind::Let,
            "print" => TokenKind::Print,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "fn" => TokenKind::Fn,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            _ => TokenKind::Identifier(s),
        };

        Token::new(kind, span)
    }
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, EngineError> {
    Lexer::new(source).tokenize()
}
