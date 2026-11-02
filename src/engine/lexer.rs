use crate::engine::token::Token;

pub fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Whitespace
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Single-line comments (//)
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // String literals ("...")
        if c == '"' {
            i += 1;
            let mut s = String::new();
            let mut terminated = false;
            while i < chars.len() {
                if chars[i] == '"' {
                    terminated = true;
                    i += 1;
                    break;
                } else if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    match chars[i] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                } else {
                    s.push(chars[i]);
                }
                i += 1;
            }
            if !terminated {
                return Err("Unterminated string literal".to_string());
            }
            tokens.push(Token::StringLiteral(s));
            continue;
        }

        // Numbers (integers and floats)
        if c.is_ascii_digit() {
            let start = i;
            let mut is_float = false;

            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }

            if i < chars.len() && chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                is_float = true;
                i += 1; // skip '.'
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }

            let text: String = chars[start..i].iter().collect();
            if is_float {
                let value = text
                    .parse::<f64>()
                    .map_err(|e| format!("Invalid float literal '{text}': {e}"))?;
                tokens.push(Token::Float(value));
            } else {
                let value = text
                    .parse::<i64>()
                    .map_err(|e| format!("Invalid integer literal '{text}': {e}"))?;
                tokens.push(Token::Number(value));
            }
            continue;
        }

        // Identifiers and Keywords
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
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
                "true" => Token::True,
                "false" => Token::False,
                _ => Token::Identifier(word),
            };

            tokens.push(token);
            continue;
        }

        // Operators & delimiters (two-character first)
        if i + 1 < chars.len() {
            let next = chars[i + 1];
            let two_char = match (c, next) {
                ('=', '=') => Some(Token::EqualEqual),
                ('!', '=') => Some(Token::BangEqual),
                ('<', '=') => Some(Token::LessEqual),
                ('>', '=') => Some(Token::GreaterEqual),
                _ => None,
            };

            if let Some(token) = two_char {
                tokens.push(token);
                i += 2;
                continue;
            }
        }

        let token = match c {
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '=' => Token::Equal,
            '<' => Token::Less,
            '>' => Token::Greater,
            '!' => Token::Bang,
            '(' => Token::LeftParen,
            ')' => Token::RightParen,
            '{' => Token::LeftBrace,
            '}' => Token::RightBrace,
            ',' => Token::Comma,
            ';' => Token::Semicolon,
            _ => return Err(format!("Unexpected character: '{c}' at position {i}")),
        };

        tokens.push(token);
        i += 1;
    }

    tokens.push(Token::Eof);
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic() {
        let src = "let x = 10 + 20.5; print(x);";
        let tokens = tokenize(src).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Identifier("x".into()),
                Token::Equal,
                Token::Number(10),
                Token::Plus,
                Token::Float(20.5),
                Token::Semicolon,
                Token::Print,
                Token::LeftParen,
                Token::Identifier("x".into()),
                Token::RightParen,
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }
}
