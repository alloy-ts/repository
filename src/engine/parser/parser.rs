use crate::engine::ast::{BinaryOp, Expr, ExprKind, Stmt, StmtKind, UnaryOp};
use crate::engine::errors::EngineError;
use crate::engine::token::{Span, Token, TokenKind};

pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn peek_span(&self) -> Span {
        self.peek().span.clone()
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.current].clone();
        if self.current < self.tokens.len() - 1 {
            self.current += 1;
        }
        token
    }

    fn consume(&mut self, expected: TokenKind, msg: &str) -> Result<Token, EngineError> {
        let current_token = self.peek().clone();
        if std::mem::discriminant(&current_token.kind) == std::mem::discriminant(&expected) {
            Ok(self.advance())
        } else {
            Err(EngineError::parse(
                format!("{msg}, got {:?}", current_token.kind),
                current_token.span.line,
                current_token.span.column,
            ))
        }
    }

    pub fn parse(&mut self) -> Result<Vec<Stmt>, EngineError> {
        let mut statements = Vec::new();

        while !matches!(self.peek_kind(), TokenKind::Eof) {
            statements.push(self.statement()?);
        }

        Ok(statements)
    }

    fn statement(&mut self) -> Result<Stmt, EngineError> {
        match self.peek_kind() {
            TokenKind::Let => self.let_decl(),
            TokenKind::Print => self.print_stmt(),
            TokenKind::Fn => self.fn_decl(),
            TokenKind::Return => self.return_stmt(),
            TokenKind::If => self.if_stmt(),
            TokenKind::While => self.while_stmt(),
            TokenKind::LeftBrace => self.block_stmt(),
            _ => self.assignment_or_expr_stmt(),
        }
    }

    fn let_decl(&mut self) -> Result<Stmt, EngineError> {
        let let_token = self.advance(); // consume 'let'
        let name_token = self.advance();
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            other => {
                return Err(EngineError::parse(
                    format!("Expected variable name after 'let', got {:?}", other),
                    name_token.span.line,
                    name_token.span.column,
                ))
            }
        };

        self.consume(TokenKind::Equal, "Expected '=' after variable name")?;
        let value = self.expression()?;
        self.consume(TokenKind::Semicolon, "Expected ';' after let declaration")?;

        Ok(Stmt::new(StmtKind::Let { name, value }, let_token.span))
    }

    fn print_stmt(&mut self) -> Result<Stmt, EngineError> {
        let print_token = self.advance(); // consume 'print'
        self.consume(TokenKind::LeftParen, "Expected '(' after 'print'")?;
        let expr = self.expression()?;
        self.consume(TokenKind::RightParen, "Expected ')' after print argument")?;
        self.consume(TokenKind::Semicolon, "Expected ';' after print statement")?;

        Ok(Stmt::new(StmtKind::Print(expr), print_token.span))
    }

    fn fn_decl(&mut self) -> Result<Stmt, EngineError> {
        let fn_token = self.advance(); // consume 'fn'
        let name_token = self.advance();
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            other => {
                return Err(EngineError::parse(
                    format!("Expected function name after 'fn', got {:?}", other),
                    name_token.span.line,
                    name_token.span.column,
                ))
            }
        };

        self.consume(TokenKind::LeftParen, "Expected '(' after function name")?;
        let mut parameters = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RightParen) {
            loop {
                let param_tok = self.advance();
                match param_tok.kind {
                    TokenKind::Identifier(p) => parameters.push(p),
                    other => {
                        return Err(EngineError::parse(
                            format!("Expected parameter identifier, got {:?}", other),
                            param_tok.span.line,
                            param_tok.span.column,
                        ))
                    }
                }

                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.consume(TokenKind::RightParen, "Expected ')' after function parameters")?;

        let body = self.block_body()?;
        Ok(Stmt::new(
            StmtKind::Function {
                name,
                parameters,
                body,
            },
            fn_token.span,
        ))
    }

    fn return_stmt(&mut self) -> Result<Stmt, EngineError> {
        let ret_token = self.advance(); // consume 'return'
        let value = if matches!(self.peek_kind(), TokenKind::Semicolon) {
            None
        } else {
            Some(self.expression()?)
        };
        self.consume(TokenKind::Semicolon, "Expected ';' after return")?;

        Ok(Stmt::new(StmtKind::Return(value), ret_token.span))
    }

    fn if_stmt(&mut self) -> Result<Stmt, EngineError> {
        let if_token = self.advance(); // consume 'if'
        self.consume(TokenKind::LeftParen, "Expected '(' after 'if'")?;
        let condition = self.expression()?;
        self.consume(TokenKind::RightParen, "Expected ')' after condition")?;

        let then_branch = self.block_body()?;
        let else_branch = if matches!(self.peek_kind(), TokenKind::Else) {
            self.advance(); // consume 'else'
            if matches!(self.peek_kind(), TokenKind::If) {
                // else if ...
                vec![self.if_stmt()?]
            } else {
                self.block_body()?
            }
        } else {
            Vec::new()
        };

        let else_opt = if else_branch.is_empty() {
            None
        } else {
            Some(else_branch)
        };

        Ok(Stmt::new(
            StmtKind::If {
                condition,
                then_branch,
                else_branch: else_opt,
            },
            if_token.span,
        ))
    }

    fn while_stmt(&mut self) -> Result<Stmt, EngineError> {
        let while_token = self.advance(); // consume 'while'
        self.consume(TokenKind::LeftParen, "Expected '(' after 'while'")?;
        let condition = self.expression()?;
        self.consume(TokenKind::RightParen, "Expected ')' after while condition")?;

        let body = self.block_body()?;
        Ok(Stmt::new(StmtKind::While { condition, body }, while_token.span))
    }

    fn block_stmt(&mut self) -> Result<Stmt, EngineError> {
        let start_span = self.peek_span();
        let body = self.block_body()?;
        Ok(Stmt::new(StmtKind::Block(body), start_span))
    }

    fn block_body(&mut self) -> Result<Vec<Stmt>, EngineError> {
        self.consume(TokenKind::LeftBrace, "Expected '{'")?;
        let mut stmts = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RightBrace | TokenKind::Eof) {
            stmts.push(self.statement()?);
        }
        self.consume(TokenKind::RightBrace, "Expected '}'")?;
        Ok(stmts)
    }

    fn assignment_or_expr_stmt(&mut self) -> Result<Stmt, EngineError> {
        // Lookahead to see if it's `IDENTIFIER = expression;`
        if let TokenKind::Identifier(name) = self.peek_kind().clone() {
            if self.current + 1 < self.tokens.len()
                && matches!(self.tokens[self.current + 1].kind, TokenKind::Equal)
            {
                let tok = self.advance(); // identifier
                self.advance(); // '='
                let value = self.expression()?;
                self.consume(TokenKind::Semicolon, "Expected ';' after assignment")?;
                return Ok(Stmt::new(StmtKind::Assign { name, value }, tok.span));
            }
        }

        let expr = self.expression()?;
        self.consume(TokenKind::Semicolon, "Expected ';' after expression")?;
        let span = expr.span.clone();
        Ok(Stmt::new(StmtKind::Expression(expr), span))
    }

    pub fn expression(&mut self) -> Result<Expr, EngineError> {
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, EngineError> {
        let mut expr = self.comparison()?;

        loop {
            let op = match self.peek_kind() {
                TokenKind::EqualEqual => BinaryOp::Equal,
                TokenKind::BangEqual => BinaryOp::NotEqual,
                _ => break,
            };

            let _op_token = self.advance();
            let right = self.comparison()?;
            let span = expr.span.clone();
            expr = Expr::new(
                ExprKind::Binary {
                    left: Box::new(expr),
                    operator: op,
                    right: Box::new(right),
                },
                span,
            );
        }

        Ok(expr)
    }

    fn comparison(&mut self) -> Result<Expr, EngineError> {
        let mut expr = self.term()?;

        loop {
            let op = match self.peek_kind() {
                TokenKind::Less => BinaryOp::Less,
                TokenKind::LessEqual => BinaryOp::LessEqual,
                TokenKind::Greater => BinaryOp::Greater,
                TokenKind::GreaterEqual => BinaryOp::GreaterEqual,
                _ => break,
            };

            self.advance();
            let right = self.term()?;
            let span = expr.span.clone();
            expr = Expr::new(
                ExprKind::Binary {
                    left: Box::new(expr),
                    operator: op,
                    right: Box::new(right),
                },
                span,
            );
        }

        Ok(expr)
    }

    fn term(&mut self) -> Result<Expr, EngineError> {
        let mut expr = self.factor()?;

        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Subtract,
                _ => break,
            };

            self.advance();
            let right = self.factor()?;
            let span = expr.span.clone();
            expr = Expr::new(
                ExprKind::Binary {
                    left: Box::new(expr),
                    operator: op,
                    right: Box::new(right),
                },
                span,
            );
        }

        Ok(expr)
    }

    fn factor(&mut self) -> Result<Expr, EngineError> {
        let mut expr = self.unary()?;

        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinaryOp::Multiply,
                TokenKind::Slash => BinaryOp::Divide,
                _ => break,
            };

            self.advance();
            let right = self.unary()?;
            let span = expr.span.clone();
            expr = Expr::new(
                ExprKind::Binary {
                    left: Box::new(expr),
                    operator: op,
                    right: Box::new(right),
                },
                span,
            );
        }

        Ok(expr)
    }

    fn unary(&mut self) -> Result<Expr, EngineError> {
        match self.peek_kind() {
            TokenKind::Minus => {
                let tok = self.advance();
                let operand = self.unary()?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        operator: UnaryOp::Negate,
                        operand: Box::new(operand),
                    },
                    tok.span,
                ))
            }
            TokenKind::Bang => {
                let tok = self.advance();
                let operand = self.unary()?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        operator: UnaryOp::Not,
                        operand: Box::new(operand),
                    },
                    tok.span,
                ))
            }
            _ => self.call_or_primary(),
        }
    }

    fn call_or_primary(&mut self) -> Result<Expr, EngineError> {
        let expr = self.primary()?;

        // If the primary expression is an identifier and is immediately followed by '(', it's a function call!
        if let ExprKind::Variable(name) = &expr.kind {
            if matches!(self.peek_kind(), TokenKind::LeftParen) {
                let name = name.clone();
                let span = expr.span.clone();
                self.advance(); // consume '('

                let mut arguments = Vec::new();
                if !matches!(self.peek_kind(), TokenKind::RightParen) {
                    loop {
                        arguments.push(self.expression()?);
                        if matches!(self.peek_kind(), TokenKind::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }

                self.consume(TokenKind::RightParen, "Expected ')' after arguments")?;
                return Ok(Expr::new(ExprKind::Call { name, arguments }, span));
            }
        }

        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, EngineError> {
        let tok = self.advance();
        match tok.kind {
            TokenKind::IntNumber(n) => Ok(Expr::new(ExprKind::Int(n), tok.span)),
            TokenKind::FloatNumber(f) => Ok(Expr::new(ExprKind::Float(f), tok.span)),
            TokenKind::StringLit(s) => Ok(Expr::new(ExprKind::StringLit(s), tok.span)),
            TokenKind::True => Ok(Expr::new(ExprKind::Bool(true), tok.span)),
            TokenKind::False => Ok(Expr::new(ExprKind::Bool(false), tok.span)),
            TokenKind::Identifier(name) => Ok(Expr::new(ExprKind::Variable(name), tok.span)),
            TokenKind::LeftParen => {
                let expr = self.expression()?;
                self.consume(TokenKind::RightParen, "Expected ')' after expression")?;
                Ok(expr)
            }
            other => Err(EngineError::parse(
                format!("Unexpected token in expression: {:?}", other),
                tok.span.line,
                tok.span.column,
            )),
        }
    }
}
