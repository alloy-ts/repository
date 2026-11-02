use crate::engine::ast::{BinaryOp, Expr, Stmt};
use crate::engine::lexer::tokenize;
use crate::engine::token::Token;

pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    fn peek(&self) -> &Token {
        if self.current < self.tokens.len() {
            &self.tokens[self.current]
        } else {
            &Token::Eof
        }
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.current < self.tokens.len() {
            self.current += 1;
        }
        token
    }

    fn check(&self, token: &Token) -> bool {
        self.peek() == token
    }

    fn consume(&mut self, expected: Token) -> Result<(), String> {
        if *self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(format!("Expected {:?}, got {:?}", expected, self.peek()))
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
            Token::LeftBrace => {
                let block = self.block()?;
                Ok(Stmt::Block(block))
            }
            Token::Identifier(_) if self.peek_ahead(1) == Some(&Token::Equal) => {
                self.assignment_statement()
            }
            _ => {
                let expr = self.expression()?;
                self.consume(Token::Semicolon)?;
                Ok(Stmt::Expression(expr))
            }
        }
    }

    fn peek_ahead(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.current + offset)
    }

    fn let_statement(&mut self) -> Result<Stmt, String> {
        self.consume(Token::Let)?;
        let name = match self.advance() {
            Token::Identifier(name) => name,
            other => return Err(format!("Expected variable name, got {:?}", other)),
        };
        self.consume(Token::Equal)?;
        let value = self.expression()?;
        self.consume(Token::Semicolon)?;
        Ok(Stmt::Let { name, value })
    }

    fn assignment_statement(&mut self) -> Result<Stmt, String> {
        let name = match self.advance() {
            Token::Identifier(name) => name,
            other => return Err(format!("Expected variable name, got {:?}", other)),
        };
        self.consume(Token::Equal)?;
        let value = self.expression()?;
        self.consume(Token::Semicolon)?;
        Ok(Stmt::Assign { name, value })
    }

    fn print_statement(&mut self) -> Result<Stmt, String> {
        self.consume(Token::Print)?;
        self.consume(Token::LeftParen)?;
        let expr = self.expression()?;
        self.consume(Token::RightParen)?;
        self.consume(Token::Semicolon)?;
        Ok(Stmt::Print(expr))
    }

    fn if_statement(&mut self) -> Result<Stmt, String> {
        self.consume(Token::If)?;
        self.consume(Token::LeftParen)?;
        let condition = self.expression()?;
        self.consume(Token::RightParen)?;

        let then_branch = self.block()?;
        let else_branch = if self.check(&Token::Else) {
            self.advance();
            if self.check(&Token::If) {
                vec![self.if_statement()?]
            } else {
                self.block()?
            }
        } else {
            Vec::new()
        };

        Ok(Stmt::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    fn while_statement(&mut self) -> Result<Stmt, String> {
        self.consume(Token::While)?;
        self.consume(Token::LeftParen)?;
        let condition = self.expression()?;
        self.consume(Token::RightParen)?;
        let body = self.block()?;
        Ok(Stmt::While { condition, body })
    }

    fn function_declaration(&mut self) -> Result<Stmt, String> {
        self.consume(Token::Fn)?;
        let name = match self.advance() {
            Token::Identifier(name) => name,
            other => return Err(format!("Expected function name, got {:?}", other)),
        };
        self.consume(Token::LeftParen)?;

        let mut parameters = Vec::new();
        if !self.check(&Token::RightParen) {
            loop {
                match self.advance() {
                    Token::Identifier(param) => parameters.push(param),
                    other => return Err(format!("Expected parameter name, got {:?}", other)),
                }
                if self.check(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.consume(Token::RightParen)?;
        let body = self.block()?;

        Ok(Stmt::Function {
            name,
            parameters,
            body,
        })
    }

    fn return_statement(&mut self) -> Result<Stmt, String> {
        self.consume(Token::Return)?;
        let value = if self.check(&Token::Semicolon) {
            None
        } else {
            Some(self.expression()?)
        };
        self.consume(Token::Semicolon)?;
        Ok(Stmt::Return(value))
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.consume(Token::LeftBrace)?;
        let mut statements = Vec::new();
        while !self.check(&Token::RightBrace) && !self.check(&Token::Eof) {
            statements.push(self.statement()?);
        }
        self.consume(Token::RightBrace)?;
        Ok(statements)
    }

    pub fn expression(&mut self) -> Result<Expr, String> {
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, String> {
        let mut expr = self.comparison()?;

        loop {
            let op = match self.peek() {
                Token::EqualEqual => BinaryOp::Equal,
                Token::BangEqual => BinaryOp::NotEqual,
                _ => break,
            };
            self.advance();
            let right = self.comparison()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
            };
        }

        Ok(expr)
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        let mut expr = self.term()?;

        loop {
            let op = match self.peek() {
                Token::Less => BinaryOp::Less,
                Token::LessEqual => BinaryOp::LessEqual,
                Token::Greater => BinaryOp::Greater,
                Token::GreaterEqual => BinaryOp::GreaterEqual,
                _ => break,
            };
            self.advance();
            let right = self.term()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
            };
        }

        Ok(expr)
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut expr = self.factor()?;

        loop {
            let op = match self.peek() {
                Token::Plus => BinaryOp::Add,
                Token::Minus => BinaryOp::Subtract,
                _ => break,
            };
            self.advance();
            let right = self.factor()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
            };
        }

        Ok(expr)
    }

    fn factor(&mut self) -> Result<Expr, String> {
        let mut expr = self.unary()?;

        loop {
            let op = match self.peek() {
                Token::Star => BinaryOp::Multiply,
                Token::Slash => BinaryOp::Divide,
                Token::Percent => BinaryOp::Modulo,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                operator: op,
                right: Box::new(right),
            };
        }

        Ok(expr)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.check(&Token::Minus) {
            self.advance();
            let right = self.unary()?;
            return Ok(Expr::Binary {
                left: Box::new(Expr::Number(0)),
                operator: BinaryOp::Subtract,
                right: Box::new(right),
            });
        }
        if self.check(&Token::Bang) {
            self.advance();
            let right = self.unary()?;
            return Ok(Expr::Binary {
                left: Box::new(right),
                operator: BinaryOp::Equal,
                right: Box::new(Expr::Bool(false)),
            });
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Token::Number(val) => {
                let v = *val;
                self.advance();
                Ok(Expr::Number(v))
            }
            Token::Float(val) => {
                let v = *val;
                self.advance();
                Ok(Expr::Float(v))
            }
            Token::StringLiteral(s) => {
                let s = s.clone();
                self.advance();
                Ok(Expr::String(s))
            }
            Token::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            Token::Identifier(name) => {
                let name = name.clone();
                self.advance();
                // Check if this is a function call
                if self.check(&Token::LeftParen) {
                    self.advance();
                    let mut arguments = Vec::new();
                    if !self.check(&Token::RightParen) {
                        loop {
                            arguments.push(self.expression()?);
                            if self.check(&Token::Comma) {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    self.consume(Token::RightParen)?;
                    Ok(Expr::Call { name, arguments })
                } else {
                    Ok(Expr::Variable(name))
                }
            }
            Token::LeftParen => {
                self.advance();
                let expr = self.expression()?;
                self.consume(Token::RightParen)?;
                Ok(expr)
            }
            other => Err(format!("Unexpected expression token: {:?}", other)),
        }
    }
}

pub fn parse(source: &str) -> Result<Vec<Stmt>, String> {
    let tokens = tokenize(source)?;
    let mut parser = Parser::new(tokens);
    parser.parse()
}
