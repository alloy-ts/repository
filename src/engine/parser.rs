use crate::engine::ast::{BinaryOp, Expr, ExprKind, Stmt, StmtKind, UnaryOp};
use crate::engine::errors::{EngineError, Span};
use crate::engine::lexer::tokenize;
use crate::engine::token::{Token, TokenKind};

pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.current.min(self.tokens.len() - 1)]
    }

    fn peek_next(&self) -> &Token {
        &self.tokens[(self.current + 1).min(self.tokens.len() - 1)]
    }

    fn is_at_end(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        &self.tokens[self.current - 1]
    }

    fn check(&self, kind: &TokenKind) -> bool {
        if self.is_at_end() {
            false
        } else {
            &self.peek().kind == kind
        }
    }

    fn consume(&mut self, kind: &TokenKind, message: &str) -> Result<&Token, EngineError> {
        if self.check(kind) {
            Ok(self.advance())
        } else {
            let token = self.peek();
            Err(EngineError::parse(
                format!("{message}, got {:?}", token.kind),
                token.span.line,
                token.span.column,
            ))
        }
    }

    pub fn parse(&mut self) -> Result<Vec<Stmt>, EngineError> {
        let mut statements = Vec::new();
        while !self.is_at_end() {
            statements.push(self.statement()?);
        }
        Ok(statements)
    }

    fn statement(&mut self) -> Result<Stmt, EngineError> {
        let token = self.peek();
        match &token.kind {
            TokenKind::Let => self.let_decl(),
            TokenKind::Fn => self.fn_decl(),
            TokenKind::Print => self.print_stmt(),
            TokenKind::If => self.if_stmt(),
            TokenKind::While => self.while_stmt(),
            TokenKind::Return => self.return_stmt(),
            TokenKind::LeftBrace => self.block_stmt(),
            TokenKind::Identifier(name) if self.peek_next().kind == TokenKind::Equal => {
                let name = name.clone();
                let span = token.span;
                self.advance(); // consume identifier
                self.advance(); // consume '='
                let value = self.expression()?;
                self.consume(&TokenKind::Semicolon, "Expected ';' after assignment")?;
                Ok(Stmt::new(StmtKind::Assign { name, value }, span))
            }
            _ => self.expression_stmt(),
        }
    }

    fn let_decl(&mut self) -> Result<Stmt, EngineError> {
        let span = self.advance().span; // consume 'let'
        let name = match &self.peek().kind {
            TokenKind::Identifier(id) => id.clone(),
            other => {
                let s = self.peek().span;
                return Err(EngineError::parse(
                    format!("Expected variable name after 'let', got {other:?}"),
                    s.line,
                    s.column,
                ));
            }
        };
        self.advance();

        self.consume(&TokenKind::Equal, "Expected '=' after variable name")?;
        let value = self.expression()?;
        self.consume(&TokenKind::Semicolon, "Expected ';' after let declaration")?;

        Ok(Stmt::new(StmtKind::Let { name, value }, span))
    }

    fn fn_decl(&mut self) -> Result<Stmt, EngineError> {
        let span = self.advance().span; // consume 'fn'
        let name = match &self.peek().kind {
            TokenKind::Identifier(id) => id.clone(),
            other => {
                let s = self.peek().span;
                return Err(EngineError::parse(
                    format!("Expected function name after 'fn', got {other:?}"),
                    s.line,
                    s.column,
                ));
            }
        };
        self.advance();

        self.consume(&TokenKind::LeftParen, "Expected '(' after function name")?;
        let mut parameters = Vec::new();
        if !self.check(&TokenKind::RightParen) {
            loop {
                match &self.peek().kind {
                    TokenKind::Identifier(param) => {
                        parameters.push(param.clone());
                        self.advance();
                    }
                    other => {
                        let s = self.peek().span;
                        return Err(EngineError::parse(
                            format!("Expected parameter name, got {other:?}"),
                            s.line,
                            s.column,
                        ));
                    }
                }

                if self.check(&TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.consume(&TokenKind::RightParen, "Expected ')' after parameters")?;

        self.consume(&TokenKind::LeftBrace, "Expected '{' before function body")?;
        let mut body = Vec::new();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            body.push(self.statement()?);
        }
        self.consume(&TokenKind::RightBrace, "Expected '}' after function body")?;

        Ok(Stmt::new(
            StmtKind::Fn {
                name,
                parameters,
                body,
            },
            span,
        ))
    }

    fn print_stmt(&mut self) -> Result<Stmt, EngineError> {
        let span = self.advance().span; // consume 'print'
        self.consume(&TokenKind::LeftParen, "Expected '(' after 'print'")?;
        let expr = self.expression()?;
        self.consume(&TokenKind::RightParen, "Expected ')' after print argument")?;
        self.consume(&TokenKind::Semicolon, "Expected ';' after print statement")?;
        Ok(Stmt::new(StmtKind::Print(expr), span))
    }

    fn if_stmt(&mut self) -> Result<Stmt, EngineError> {
        let span = self.advance().span; // consume 'if'
        self.consume(&TokenKind::LeftParen, "Expected '(' after 'if'")?;
        let condition = self.expression()?;
        self.consume(&TokenKind::RightParen, "Expected ')' after condition")?;

        let then_branch = Box::new(self.block_stmt()?);
        let else_branch = if self.check(&TokenKind::Else) {
            self.advance(); // consume 'else'
            if self.check(&TokenKind::If) {
                Some(Box::new(self.if_stmt()?))
            } else {
                Some(Box::new(self.block_stmt()?))
            }
        } else {
            None
        };

        Ok(Stmt::new(
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            },
            span,
        ))
    }

    fn while_stmt(&mut self) -> Result<Stmt, EngineError> {
        let span = self.advance().span; // consume 'while'
        self.consume(&TokenKind::LeftParen, "Expected '(' after 'while'")?;
        let condition = self.expression()?;
        self.consume(&TokenKind::RightParen, "Expected ')' after condition")?;
        let body = Box::new(self.block_stmt()?);

        Ok(Stmt::new(StmtKind::While { condition, body }, span))
    }

    fn return_stmt(&mut self) -> Result<Stmt, EngineError> {
        let span = self.advance().span; // consume 'return'
        let value = if self.check(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.expression()?)
        };
        self.consume(&TokenKind::Semicolon, "Expected ';' after return")?;
        Ok(Stmt::new(StmtKind::Return(value), span))
    }

    fn block_stmt(&mut self) -> Result<Stmt, EngineError> {
        let span = self.consume(&TokenKind::LeftBrace, "Expected '{' to start block")?.span;
        let mut stmts = Vec::new();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            stmts.push(self.statement()?);
        }
        self.consume(&TokenKind::RightBrace, "Expected '}' after block")?;
        Ok(Stmt::new(StmtKind::Block(stmts), span))
    }

    fn expression_stmt(&mut self) -> Result<Stmt, EngineError> {
        let expr = self.expression()?;
        let span = expr.span;
        self.consume(&TokenKind::Semicolon, "Expected ';' after expression")?;
        Ok(Stmt::new(StmtKind::Expression(expr), span))
    }

    pub fn expression(&mut self) -> Result<Expr, EngineError> {
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, EngineError> {
        let mut expr = self.comparison()?;

        while self.check(&TokenKind::EqualEqual) || self.check(&TokenKind::BangEqual) {
            let op_token = self.advance().clone();
            let op = match op_token.kind {
                TokenKind::EqualEqual => BinaryOp::Equal,
                TokenKind::BangEqual => BinaryOp::NotEqual,
                _ => unreachable!(),
            };
            let right = self.comparison()?;
            let span = expr.span;
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

        while self.check(&TokenKind::Less)
            || self.check(&TokenKind::LessEqual)
            || self.check(&TokenKind::Greater)
            || self.check(&TokenKind::GreaterEqual)
        {
            let op_token = self.advance().clone();
            let op = match op_token.kind {
                TokenKind::Less => BinaryOp::Less,
                TokenKind::LessEqual => BinaryOp::LessEqual,
                TokenKind::Greater => BinaryOp::Greater,
                TokenKind::GreaterEqual => BinaryOp::GreaterEqual,
                _ => unreachable!(),
            };
            let right = self.term()?;
            let span = expr.span;
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

        while self.check(&TokenKind::Plus) || self.check(&TokenKind::Minus) {
            let op_token = self.advance().clone();
            let op = match op_token.kind {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Subtract,
                _ => unreachable!(),
            };
            let right = self.factor()?;
            let span = expr.span;
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

        while self.check(&TokenKind::Star) || self.check(&TokenKind::Slash) {
            let op_token = self.advance().clone();
            let op = match op_token.kind {
                TokenKind::Star => BinaryOp::Multiply,
                TokenKind::Slash => BinaryOp::Divide,
                _ => unreachable!(),
            };
            let right = self.unary()?;
            let span = expr.span;
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
        if self.check(&TokenKind::Minus) {
            let span = self.advance().span;
            let operand = self.unary()?;
            return Ok(Expr::new(
                ExprKind::Unary {
                    operator: UnaryOp::Negate,
                    operand: Box::new(operand),
                },
                span,
            ));
        }
        if self.check(&TokenKind::Bang) {
            let span = self.advance().span;
            let operand = self.unary()?;
            return Ok(Expr::new(
                ExprKind::Unary {
                    operator: UnaryOp::Not,
                    operand: Box::new(operand),
                },
                span,
            ));
        }

        self.call()
    }

    fn call(&mut self) -> Result<Expr, EngineError> {
        let mut expr = self.primary()?;

        while self.check(&TokenKind::LeftParen) {
            let span = expr.span;
            self.advance(); // consume '('
            let mut arguments = Vec::new();

            if !self.check(&TokenKind::RightParen) {
                loop {
                    arguments.push(self.expression()?);
                    if self.check(&TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }

            self.consume(&TokenKind::RightParen, "Expected ')' after arguments")?;
            expr = Expr::new(
                ExprKind::Call {
                    callee: Box::new(expr),
                    arguments,
                },
                span,
            );
        }

        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, EngineError> {
        let token = self.peek().clone();
        let span = token.span;

        match token.kind {
            TokenKind::Int(v) => {
                self.advance();
                Ok(Expr::new(ExprKind::Int(v), span))
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(Expr::new(ExprKind::Float(v), span))
            }
            TokenKind::String(ref s) => {
                let s = s.clone();
                self.advance();
                Ok(Expr::new(ExprKind::String(s), span))
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::new(ExprKind::Bool(true), span))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::new(ExprKind::Bool(false), span))
            }
            TokenKind::Identifier(ref id) => {
                let id = id.clone();
                self.advance();
                Ok(Expr::new(ExprKind::Variable(id), span))
            }
            TokenKind::LeftParen => {
                self.advance();
                let expr = self.expression()?;
                self.consume(&TokenKind::RightParen, "Expected ')' after expression")?;
                Ok(expr)
            }
            other => Err(EngineError::parse(
                format!("Unexpected token in expression: {other:?}"),
                span.line,
                span.column,
            )),
        }
    }
}

pub fn parse(source: &str) -> Result<Vec<Stmt>, EngineError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser::new(tokens);
    parser.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_arithmetic() {
        let ast = parse("10 + 20 * 2;").expect("parse failed");
        assert_eq!(ast.len(), 1);
        match &ast[0].kind {
            StmtKind::Expression(expr) => match &expr.kind {
                ExprKind::Binary { operator, .. } => assert_eq!(*operator, BinaryOp::Add),
                _ => panic!("expected binary expr"),
            },
            _ => panic!("expected expression stmt"),
        }
    }

    #[test]
    fn test_parse_while_loop() {
        let source = r#"
            let sum = 0;
            let i = 1;
            while (i <= 100) {
                sum = sum + i;
                i = i + 1;
            }
            print(sum);
        "#;
        let ast = parse(source).expect("parse while loop");
        assert_eq!(ast.len(), 4);
    }

    #[test]
    fn test_parse_fn_and_call() {
        let source = r#"
            fn factorial(n) {
                if (n <= 1) {
                    return 1;
                }
                return n * factorial(n - 1);
            }
            print(factorial(5));
        "#;
        let ast = parse(source).expect("parse fn");
        assert_eq!(ast.len(), 2);
    }

    #[test]
    fn test_parse_errors() {
        assert!(parse("let = 10;").is_err());
        assert!(parse("1 + ;").is_err());
        assert!(parse("fn (x) {}").is_err());
    }
}
