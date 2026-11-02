//! Recursive-descent parser with operator precedence for Engine.

use crate::engine::ast::{BinaryOp, Expr, Stmt, UnaryOp};
use crate::engine::token::{Span, SpannedToken, Token};

pub struct Parser {
    tokens: Vec<SpannedToken>,
    current: usize,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self { tokens, current: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.current].token
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.current].span
    }

    fn advance(&mut self) -> SpannedToken {
        let token = self.tokens[self.current].clone();
        if self.current + 1 < self.tokens.len() {
            self.current += 1;
        }
        token
    }

    fn consume(&mut self, expected: Token) -> Result<SpannedToken, String> {
        if *self.peek() == expected {
            Ok(self.advance())
        } else {
            Err(format!(
                "Expected {:?}, got {:?} at line {}, column {}",
                expected,
                self.peek(),
                self.peek_span().line,
                self.peek_span().column
            ))
        }
    }

    pub fn parse(&mut self) -> Result<Vec<Stmt>, String> {
        let mut statements = Vec::new();
        while *self.peek() != Token::Eof {
            statements.push(self.statement()?);
        }
        Ok(statements)
    }

    fn statement(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Token::Let => self.let_statement(),
            Token::Print => self.print_statement(),
            Token::If => self.if_statement(),
            Token::While => self.while_statement(),
            Token::Fn => self.function_declaration(),
            Token::Return => self.return_statement(),
            Token::LeftBrace => self.block_statement(),
            Token::Identifier(_) => {
                // Peek ahead to see if it's an assignment (IDENT = ...) or call/expr
                if self.current + 1 < self.tokens.len() && self.tokens[self.current + 1].token == Token::Equal {
                    self.assignment_statement()
                } else {
                    self.expression_statement()
                }
            }
            _ => self.expression_statement(),
        }
    }

    fn let_statement(&mut self) -> Result<Stmt, String> {
        let let_tok = self.advance();
        let name = match self.advance().token {
            Token::Identifier(n) => n,
            other => {
                return Err(format!(
                    "Expected variable identifier after 'let', got {:?} at line {}, column {}",
                    other, let_tok.span.line, let_tok.span.column
                ))
            }
        };

        self.consume(Token::Equal)?;
        let value = self.expression()?;
        self.consume(Token::Semicolon)?;

        Ok(Stmt::Let {
            name,
            value,
            span: let_tok.span,
        })
    }

    fn assignment_statement(&mut self) -> Result<Stmt, String> {
        let ident_tok = self.advance();
        let name = match ident_tok.token {
            Token::Identifier(n) => n,
            _ => unreachable!(),
        };

        self.consume(Token::Equal)?;
        let value = self.expression()?;
        self.consume(Token::Semicolon)?;

        Ok(Stmt::Assign {
            name,
            value,
            span: ident_tok.span,
        })
    }

    fn print_statement(&mut self) -> Result<Stmt, String> {
        let print_tok = self.advance();
        self.consume(Token::LeftParen)?;
        let expr = self.expression()?;
        self.consume(Token::RightParen)?;
        self.consume(Token::Semicolon)?;

        Ok(Stmt::Print(expr, print_tok.span))
    }

    fn if_statement(&mut self) -> Result<Stmt, String> {
        let if_tok = self.advance();
        self.consume(Token::LeftParen)?;
        let condition = self.expression()?;
        self.consume(Token::RightParen)?;

        let then_branch = self.block_body()?;
        let mut else_branch = Vec::new();

        if *self.peek() == Token::Else {
            self.advance();
            if *self.peek() == Token::If {
                else_branch.push(self.if_statement()?);
            } else {
                else_branch = self.block_body()?;
            }
        }

        Ok(Stmt::If {
            condition,
            then_branch,
            else_branch,
            span: if_tok.span,
        })
    }

    fn while_statement(&mut self) -> Result<Stmt, String> {
        let while_tok = self.advance();
        self.consume(Token::LeftParen)?;
        let condition = self.expression()?;
        self.consume(Token::RightParen)?;

        let body = self.block_body()?;

        Ok(Stmt::While {
            condition,
            body,
            span: while_tok.span,
        })
    }

    fn function_declaration(&mut self) -> Result<Stmt, String> {
        let fn_tok = self.advance();
        let name = match self.advance().token {
            Token::Identifier(n) => n,
            other => return Err(format!("Expected function name, got {:?}", other)),
        };

        self.consume(Token::LeftParen)?;
        let mut parameters = Vec::new();

        if *self.peek() != Token::RightParen {
            loop {
                match self.advance().token {
                    Token::Identifier(p) => parameters.push(p),
                    other => return Err(format!("Expected parameter name, got {:?}", other)),
                }

                if *self.peek() == Token::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }

        self.consume(Token::RightParen)?;
        let body = self.block_body()?;

        Ok(Stmt::Function {
            name,
            parameters,
            body,
            span: fn_tok.span,
        })
    }

    fn return_statement(&mut self) -> Result<Stmt, String> {
        let ret_tok = self.advance();
        let val = if *self.peek() == Token::Semicolon {
            None
        } else {
            Some(self.expression()?)
        };
        self.consume(Token::Semicolon)?;

        Ok(Stmt::Return(val, ret_tok.span))
    }

    fn block_statement(&mut self) -> Result<Stmt, String> {
        let span = self.peek_span();
        let body = self.block_body()?;
        Ok(Stmt::Block(body, span))
    }

    fn block_body(&mut self) -> Result<Vec<Stmt>, String> {
        self.consume(Token::LeftBrace)?;
        let mut statements = Vec::new();

        while *self.peek() != Token::RightBrace && *self.peek() != Token::Eof {
            statements.push(self.statement()?);
        }

        self.consume(Token::RightBrace)?;
        Ok(statements)
    }

    fn expression_statement(&mut self) -> Result<Stmt, String> {
        let expr = self.expression()?;
        let span = expr.span();
        self.consume(Token::Semicolon)?;
        Ok(Stmt::Expression(expr, span))
    }

    pub fn expression(&mut self) -> Result<Expr, String> {
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, String> {
        let mut expr = self.comparison()?;

        loop {
            let (op, span) = match self.peek() {
                Token::EqualEqual => (BinaryOp::Equal, self.peek_span()),
                Token::BangEqual => (BinaryOp::NotEqual, self.peek_span()),
                _ => break,
            };

            self.advance();
            let right = self.comparison()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        let mut expr = self.term()?;

        loop {
            let (op, span) = match self.peek() {
                Token::Less => (BinaryOp::Less, self.peek_span()),
                Token::LessEqual => (BinaryOp::LessEqual, self.peek_span()),
                Token::Greater => (BinaryOp::Greater, self.peek_span()),
                Token::GreaterEqual => (BinaryOp::GreaterEqual, self.peek_span()),
                _ => break,
            };

            self.advance();
            let right = self.term()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut expr = self.factor()?;

        loop {
            let (op, span) = match self.peek() {
                Token::Plus => (BinaryOp::Add, self.peek_span()),
                Token::Minus => (BinaryOp::Subtract, self.peek_span()),
                _ => break,
            };

            self.advance();
            let right = self.factor()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn factor(&mut self) -> Result<Expr, String> {
        let mut expr = self.unary()?;

        loop {
            let (op, span) = match self.peek() {
                Token::Star => (BinaryOp::Multiply, self.peek_span()),
                Token::Slash => (BinaryOp::Divide, self.peek_span()),
                _ => break,
            };

            self.advance();
            let right = self.unary()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Token::Minus => {
                let span = self.peek_span();
                self.advance();
                let operand = self.unary()?;
                Ok(Expr::Unary {
                    operator: UnaryOp::Negate,
                    operand: Box::new(operand),
                    span,
                })
            }
            Token::Bang => {
                let span = self.peek_span();
                self.advance();
                let operand = self.unary()?;
                Ok(Expr::Unary {
                    operator: UnaryOp::Not,
                    operand: Box::new(operand),
                    span,
                })
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let token = self.advance();
        match token.token {
            Token::Number(val) => Ok(Expr::Number(val, token.span)),
            Token::Float(val) => Ok(Expr::Float(val, token.span)),
            Token::StringLit(s) => Ok(Expr::StringLit(s, token.span)),
            Token::Boolean(b) => Ok(Expr::Boolean(b, token.span)),
            Token::Identifier(name) => {
                if *self.peek() == Token::LeftParen {
                    // Function call
                    self.advance();
                    let mut arguments = Vec::new();
                    if *self.peek() != Token::RightParen {
                        loop {
                            arguments.push(self.expression()?);
                            if *self.peek() == Token::Comma {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    self.consume(Token::RightParen)?;
                    Ok(Expr::Call {
                        name,
                        arguments,
                        span: token.span,
                    })
                } else {
                    Ok(Expr::Variable(name, token.span))
                }
            }
            Token::LeftParen => {
                let expr = self.expression()?;
                self.consume(Token::RightParen)?;
                Ok(expr)
            }
            other => Err(format!(
                "Unexpected expression token {:?} at line {}, column {}",
                other, token.span.line, token.span.column
            )),
        }
    }
}
