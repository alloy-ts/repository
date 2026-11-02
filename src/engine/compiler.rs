//! Compiler translating Engine AST statements into FuseVM Bytecode chunks.

use std::collections::HashMap;
use std::sync::Arc;
use fusevm::{Chunk, ChunkBuilder, Op, Value};

use crate::engine::ast::{BinaryOp, Expr, Stmt, UnaryOp};
use crate::engine::symbols::SymbolTable;

pub const EXT_PRINT: u16 = 0;
pub const EXT_HOST_CALL_BASE: u16 = 1000;

pub struct CompiledProgram {
    pub chunk: Chunk,
    pub global_slots: HashMap<String, u16>,
    pub uses_host_functions: bool,
}

pub struct Compiler {
    builder: ChunkBuilder,
    symbols: SymbolTable,
    host_functions: HashMap<String, u16>,
    uses_host_functions: bool,
}

impl Compiler {
    pub fn new(host_functions: HashMap<String, u16>, known_globals: &[String]) -> Self {
        let mut symbols = SymbolTable::new();
        for g in known_globals {
            let _ = symbols.declare(g);
        }

        Self {
            builder: ChunkBuilder::new(),
            symbols,
            host_functions,
            uses_host_functions: false,
        }
    }

    pub fn compile(mut self, statements: &[Stmt]) -> Result<CompiledProgram, String> {
        let count = statements.len();
        for (i, statement) in statements.iter().enumerate() {
            let is_last = i + 1 == count;
            self.compile_stmt(statement, is_last)?;
        }

        let global_slots = self.symbols.get_root_declarations();

        Ok(CompiledProgram {
            chunk: self.builder.build(),
            global_slots,
            uses_host_functions: self.uses_host_functions,
        })
    }

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let slot = self.symbols.declare(name)?;
                self.compile_expr(value)?;
                self.builder.emit(Op::SetSlot(slot), 1);
            }

            Stmt::Assign { name, value, .. } => {
                let slot = self.symbols.resolve(name)?;
                self.compile_expr(value)?;
                self.builder.emit(Op::SetSlot(slot), 1);
            }

            Stmt::Print(expr, ..) => {
                self.compile_expr(expr)?;
                // Lower print to an extension call so output can be captured into stdout
                self.builder.emit(Op::Extended(EXT_PRINT, 1), 1);
            }

            Stmt::While { condition, body, .. } => {
                let loop_start = self.builder.current_pos();
                self.compile_expr(condition)?;

                let exit_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), 1);

                self.symbols.enter_scope();
                for (j, s) in body.iter().enumerate() {
                    self.compile_stmt(s, j + 1 == body.len())?;
                }
                self.symbols.exit_scope();

                self.builder.emit(Op::Jump(loop_start), 1);

                let exit = self.builder.current_pos();
                self.builder.patch_jump(exit_jump, exit);
            }

            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.compile_expr(condition)?;
                let else_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), 1);

                self.symbols.enter_scope();
                for (j, s) in then_branch.iter().enumerate() {
                    self.compile_stmt(s, j + 1 == then_branch.len())?;
                }
                self.symbols.exit_scope();

                if !else_branch.is_empty() {
                    let end_jump = self.builder.emit(Op::Jump(usize::MAX), 1);
                    let else_start = self.builder.current_pos();
                    self.builder.patch_jump(else_jump, else_start);

                    self.symbols.enter_scope();
                    for (j, s) in else_branch.iter().enumerate() {
                        self.compile_stmt(s, j + 1 == else_branch.len())?;
                    }
                    self.symbols.exit_scope();

                    let end = self.builder.current_pos();
                    self.builder.patch_jump(end_jump, end);
                } else {
                    let end = self.builder.current_pos();
                    self.builder.patch_jump(else_jump, end);
                }
            }

            Stmt::Block(statements, ..) => {
                self.symbols.enter_scope();
                for (j, s) in statements.iter().enumerate() {
                    self.compile_stmt(s, j + 1 == statements.len())?;
                }
                self.symbols.exit_scope();
            }

            Stmt::Expression(expr, ..) => {
                self.compile_expr(expr)?;
                // Preserve value on stack for the final expression statement of the program
                if !is_last {
                    self.builder.emit(Op::Pop, 1);
                }
            }

            Stmt::Function { .. } => {
                return Err("Function declarations are compiled in function contexts".to_string());
            }

            Stmt::Return(val, ..) => {
                if let Some(expr) = val {
                    self.compile_expr(expr)?;
                } else {
                    self.builder.emit(Op::LoadInt(0), 1);
                }
            }
        }

        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), String> {
        match expr {
            Expr::Number(val, ..) => {
                self.builder.emit(Op::LoadInt(*val), 1);
            }

            Expr::Float(val, ..) => {
                self.builder.emit(Op::LoadFloat(*val), 1);
            }

            Expr::StringLit(s, ..) => {
                let idx = self.builder.add_constant(Value::Str(Arc::new(s.clone())));
                self.builder.emit(Op::LoadConst(idx), 1);
            }

            Expr::Boolean(b, ..) => {
                if *b {
                    self.builder.emit(Op::LoadTrue, 1);
                } else {
                    self.builder.emit(Op::LoadFalse, 1);
                }
            }

            Expr::Variable(name, ..) => {
                let slot = self.symbols.resolve(name)?;
                self.builder.emit(Op::GetSlot(slot), 1);
            }

            Expr::Unary {
                operator, operand, ..
            } => {
                self.compile_expr(operand)?;
                match operator {
                    UnaryOp::Negate => {
                        self.builder.emit(Op::LoadInt(-1), 1);
                        self.builder.emit(Op::Mul, 1);
                    }
                    UnaryOp::Not => {
                        self.builder.emit(Op::LoadFalse, 1);
                        self.builder.emit(Op::NumEq, 1);
                    }
                }
            }

            Expr::Binary {
                left,
                operator,
                right,
                ..
            } => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;

                match operator {
                    BinaryOp::Add => {
                        self.builder.emit(Op::Add, 1);
                    }
                    BinaryOp::Subtract => {
                        self.builder.emit(Op::Sub, 1);
                    }
                    BinaryOp::Multiply => {
                        self.builder.emit(Op::Mul, 1);
                    }
                    BinaryOp::Divide => {
                        self.builder.emit(Op::Div, 1);
                    }
                    BinaryOp::Less => {
                        self.builder.emit(Op::NumLt, 1);
                    }
                    BinaryOp::LessEqual => {
                        self.builder.emit(Op::NumLe, 1);
                    }
                    BinaryOp::Greater => {
                        self.builder.emit(Op::NumGt, 1);
                    }
                    BinaryOp::GreaterEqual => {
                        self.builder.emit(Op::NumGe, 1);
                    }
                    BinaryOp::Equal => {
                        self.builder.emit(Op::NumEq, 1);
                    }
                    BinaryOp::NotEqual => {
                        self.builder.emit(Op::NumNe, 1);
                    }
                }
            }

            Expr::Call {
                name, arguments, span,
            } => {
                if let Some(&ext_id) = self.host_functions.get(name) {
                    self.uses_host_functions = true;
                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }
                    let argc = arguments.len() as u8;
                    self.builder.emit(Op::Extended(ext_id, argc), 1);
                } else {
                    return Err(format!(
                        "Function '{name}' not found at line {}, column {}",
                        span.line, span.column
                    ));
                }
            }
        }

        Ok(())
    }
}
