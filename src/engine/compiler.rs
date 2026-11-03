use std::collections::HashSet;
use fusevm::{Chunk, ChunkBuilder, Op, Value};

use crate::engine::ast::{BinaryOp, Expr, ExprKind, Stmt, StmtKind, UnaryOp};
use crate::engine::errors::EngineError;
use crate::engine::functions::FunctionTable;
use crate::engine::symbols::SymbolTable;

pub const EXT_HOST_CALL_BASE: u16 = 1000;

pub struct Compiler {
    builder: ChunkBuilder,
    symbols: SymbolTable,
    functions: FunctionTable,
    host_functions: HashSet<String>,
    in_function: bool,
    global_names: Vec<String>,
}

impl Compiler {
    pub fn new(host_functions: HashSet<String>, global_names: Vec<String>) -> Self {
        Self {
            builder: ChunkBuilder::new(),
            symbols: SymbolTable::new(),
            functions: FunctionTable::new(),
            host_functions,
            in_function: false,
            global_names,
        }
    }

    pub fn compile(mut self, statements: &[Stmt]) -> Result<Chunk, EngineError> {
        // Pass 1: Collect and check function declarations
        for stmt in statements {
            if let StmtKind::Function { name, parameters, .. } = &stmt.kind {
                // Reserve name in ChunkBuilder
                let _ = self.builder.add_name(name);
                self.functions
                    .declare(name.clone(), 0, parameters.clone())
                    .map_err(|e| EngineError::compile(e, stmt.span.line, stmt.span.column))?;
            }
        }

        // Jump over function bodies directly to main
        let jump_to_main = self.builder.emit(Op::Jump(usize::MAX), 1);

        // Compile functions
        for stmt in statements {
            if let StmtKind::Function {
                name,
                parameters,
                body,
            } = &stmt.kind
            {
                self.compile_function(name, parameters, body, stmt.span.line, stmt.span.column)?;
            }
        }

        // Main entry point
        let main_start = self.builder.current_pos();
        self.builder.patch_jump(jump_to_main, main_start);

        // Preload globals into global slots / names
        // FuseVM `Op::GetVar(idx)` / `Op::SetVar(idx)` uses name pool index.
        for g_name in &self.global_names {
            let _ = self.builder.add_name(g_name);
        }

        // Compile top-level statements (excluding function declarations)
        let mut last_was_expr = false;
        for stmt in statements {
            if matches!(stmt.kind, StmtKind::Function { .. }) {
                continue;
            }
            if matches!(stmt.kind, StmtKind::Expression(_)) {
                last_was_expr = true;
            } else {
                last_was_expr = false;
            }
            self.compile_stmt(stmt, last_was_expr)?;
        }

        // If the last top-level statement was not an expression, push Undef as final result
        if !last_was_expr {
            self.builder.emit(Op::LoadUndef, 1);
        }

        Ok(self.builder.build())
    }

    fn compile_function(
        &mut self,
        name: &str,
        parameters: &[String],
        body: &[Stmt],
        line: usize,
        col: usize,
    ) -> Result<(), EngineError> {
        let name_idx = self.builder.add_name(name);
        let entry_ip = self.builder.current_pos();
        self.builder.add_sub_entry(name_idx, entry_ip);

        // New scope for function parameters and local variables
        let prev_symbols = std::mem::replace(&mut self.symbols, SymbolTable::new());
        self.in_function = true;

        for param in parameters {
            self.symbols
                .declare(param)
                .map_err(|e| EngineError::compile(e, line, col))?;
        }

        for param_idx in (0..parameters.len()).rev() {
            self.builder.emit(Op::SetSlot(param_idx as u16), line as u32);
        }

        let mut has_explicit_return = false;
        for stmt in body {
            if matches!(stmt.kind, StmtKind::Return(_)) {
                has_explicit_return = true;
            }
            self.compile_stmt(stmt, false)?;
        }

        if !has_explicit_return {
            self.builder.emit(Op::LoadUndef, line as u32);
            self.builder.emit(Op::ReturnValue, line as u32);
        }

        self.in_function = false;
        self.symbols = prev_symbols;
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt, is_last_expr: bool) -> Result<(), EngineError> {
        let line = stmt.span.line as u32;

        match &stmt.kind {
            StmtKind::Let { name, value } => {
                let slot = self
                    .symbols
                    .declare(name)
                    .map_err(|e| EngineError::compile(e, stmt.span.line, stmt.span.column))?;
                self.compile_expr(value)?;
                self.builder.emit(Op::SetSlot(slot), line);
            }

            StmtKind::Assign { name, value } => {
                if let Ok(slot) = self.symbols.resolve(name) {
                    self.compile_expr(value)?;
                    self.builder.emit(Op::SetSlot(slot), line);
                } else if self.global_names.contains(name) {
                    let name_idx = self.builder.add_name(name);
                    self.compile_expr(value)?;
                    self.builder.emit(Op::SetVar(name_idx), line);
                } else {
                    return Err(EngineError::compile(
                        format!("Assignment to undeclared variable: {name}"),
                        stmt.span.line,
                        stmt.span.column,
                    ));
                }
            }

            StmtKind::Print(expr) => {
                self.compile_expr(expr)?;
                self.builder.emit(Op::PrintLn(1), line);
            }

            StmtKind::Return(opt_expr) => {
                if !self.in_function {
                    return Err(EngineError::compile(
                        "Return statement outside of function",
                        stmt.span.line,
                        stmt.span.column,
                    ));
                }
                match opt_expr {
                    Some(expr) => {
                        self.compile_expr(expr)?;
                        self.builder.emit(Op::ReturnValue, line);
                    }
                    None => {
                        self.builder.emit(Op::LoadUndef, line);
                        self.builder.emit(Op::ReturnValue, line);
                    }
                }
            }

            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.compile_expr(condition)?;
                let else_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), line);

                self.symbols.enter_scope();
                for s in then_branch {
                    self.compile_stmt(s, false)?;
                }
                self.symbols.exit_scope();

                if let Some(else_stmts) = else_branch {
                    let end_jump = self.builder.emit(Op::Jump(usize::MAX), line);
                    let else_start = self.builder.current_pos();
                    self.builder.patch_jump(else_jump, else_start);

                    self.symbols.enter_scope();
                    for s in else_stmts {
                        self.compile_stmt(s, false)?;
                    }
                    self.symbols.exit_scope();

                    let end = self.builder.current_pos();
                    self.builder.patch_jump(end_jump, end);
                } else {
                    let end = self.builder.current_pos();
                    self.builder.patch_jump(else_jump, end);
                }
            }

            StmtKind::While { condition, body } => {
                let loop_start = self.builder.current_pos();
                self.compile_expr(condition)?;
                let exit_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), line);

                self.symbols.enter_scope();
                for s in body {
                    self.compile_stmt(s, false)?;
                }
                self.symbols.exit_scope();

                self.builder.emit(Op::Jump(loop_start), line);
                let loop_exit = self.builder.current_pos();
                self.builder.patch_jump(exit_jump, loop_exit);
            }

            StmtKind::Block(stmts) => {
                self.symbols.enter_scope();
                for s in stmts {
                    self.compile_stmt(s, false)?;
                }
                self.symbols.exit_scope();
            }

            StmtKind::Expression(expr) => {
                self.compile_expr(expr)?;
                if !is_last_expr {
                    self.builder.emit(Op::Pop, line);
                }
            }

            StmtKind::Function { .. } => {}
        }

        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), EngineError> {
        let line = expr.span.line as u32;

        match &expr.kind {
            ExprKind::Int(val) => {
                self.builder.emit(Op::LoadInt(*val), line);
            }

            ExprKind::Float(val) => {
                self.builder.emit(Op::LoadFloat(*val), line);
            }

            ExprKind::StringLit(s) => {
                let const_idx = self.builder.add_constant(Value::str(s.clone()));
                self.builder.emit(Op::LoadConst(const_idx), line);
            }

            ExprKind::Bool(b) => {
                if *b {
                    self.builder.emit(Op::LoadTrue, line);
                } else {
                    self.builder.emit(Op::LoadFalse, line);
                }
            }

            ExprKind::Variable(name) => {
                if let Ok(slot) = self.symbols.resolve(name) {
                    self.builder.emit(Op::GetSlot(slot), line);
                } else {
                    // Fall back to named variable/global
                    let name_idx = self.builder.add_name(name);
                    self.builder.emit(Op::GetVar(name_idx), line);
                }
            }

            ExprKind::Unary { operator, operand } => {
                self.compile_expr(operand)?;
                match operator {
                    UnaryOp::Negate => {
                        self.builder.emit(Op::Negate, line);
                    }
                    UnaryOp::Not => {
                        self.builder.emit(Op::LogNot, line);
                    }
                }
            }

            ExprKind::Binary {
                left,
                operator,
                right,
            } => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;

                let op = match operator {
                    BinaryOp::Add => Op::Add,
                    BinaryOp::Subtract => Op::Sub,
                    BinaryOp::Multiply => Op::Mul,
                    BinaryOp::Divide => Op::Div,
                    BinaryOp::Less => Op::NumLt,
                    BinaryOp::LessEqual => Op::NumLe,
                    BinaryOp::Greater => Op::NumGt,
                    BinaryOp::GreaterEqual => Op::NumGe,
                    BinaryOp::Equal => Op::NumEq,
                    BinaryOp::NotEqual => Op::NumNe,
                };
                self.builder.emit(op, line);
            }

            ExprKind::Call { name, arguments } => {
                if let Ok(fn_info) = self.functions.resolve(name) {
                    if fn_info.parameters.len() != arguments.len() {
                        return Err(EngineError::compile(
                            format!(
                                "Function '{}' expects {} arguments, got {}",
                                name,
                                fn_info.parameters.len(),
                                arguments.len()
                            ),
                            expr.span.line,
                            expr.span.column,
                        ));
                    }
                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }
                    let name_idx = self.builder.add_name(name);
                    self.builder
                        .emit(Op::Call(name_idx, arguments.len() as u8), line);
                } else if self.host_functions.contains(name) {
                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }
                    let name_idx = self.builder.add_name(name);
                    self.builder.emit(
                        Op::Extended(EXT_HOST_CALL_BASE + name_idx, arguments.len() as u8),
                        line,
                    );
                } else {
                    return Err(EngineError::compile(
                        format!("Call to undefined function: '{name}'"),
                        expr.span.line,
                        expr.span.column,
                    ));
                }
            }
        }

        Ok(())
    }
}
