//! AST definitions for the Engine language.

use crate::engine::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Negate,
    Not,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(i64, Span),
    Float(f64, Span),
    StringLit(String, Span),
    Boolean(bool, Span),
    Variable(String, Span),
    Unary {
        operator: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Binary {
        left: Box<Expr>,
        operator: BinaryOp,
        right: Box<Expr>,
        span: Span,
    },
    Call {
        name: String,
        arguments: Vec<Expr>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Number(_, s) => *s,
            Expr::Float(_, s) => *s,
            Expr::StringLit(_, s) => *s,
            Expr::Boolean(_, s) => *s,
            Expr::Variable(_, s) => *s,
            Expr::Unary { span, .. } => *span,
            Expr::Binary { span, .. } => *span,
            Expr::Call { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        value: Expr,
        span: Span,
    },
    Assign {
        name: String,
        value: Expr,
        span: Span,
    },
    Print(Expr, Span),
    Return(Option<Expr>, Span),
    Function {
        name: String,
        parameters: Vec<String>,
        body: Vec<Stmt>,
        span: Span,
    },
    If {
        condition: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Vec<Stmt>,
        span: Span,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Block(Vec<Stmt>, Span),
    Expression(Expr, Span),
}
