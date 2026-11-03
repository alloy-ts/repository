use std::collections::HashMap;
use std::sync::Arc;

use fusevm::{Chunk, ChunkBuilder, Op, Value};

use crate::engine::ast::{BinaryOp, Expr, ExprKind, Stmt, StmtKind, UnaryOp};
use crate::engine::errors::EngineError;
use crate::engine::functions::FunctionTable;
use crate::engine::symbols::SymbolTable;

pub const EXT_HOST_BASE: u16 = 1000;

#[derive(Debug, Clone)]
pub struct CompiledProgram {
    pub chunk: Arc<Chunk>,
    pub globals: HashMap<String, u16>,
    pub uses_host_functions: bool,
}

pub struct Compiler<'a> {
    builder: ChunkBuilder,
    symbols: SymbolTable,
    functions: FunctionTable,
    host_functions: &'a HashMap<String, u16>,
    uses_host_functions: bool,
    in_function: bool,
}

impl<'a> Compiler<'a> {
    pub fn new(host_functions: &'a HashMap<String, u16>) -> Self {
        Self {
            builder: ChunkBuilder::new(),
            symbols: SymbolTable::new(),
            functions: FunctionTable::new(),
            host_functions,
            uses_host_functions: false,
            in_function: false,
        }
    }

    pub fn compile(mut self, statements: &[Stmt]) -> Result<CompiledProgram, EngineError> {
        // Pass 1: Collect user function declarations
        for stmt in statements {
            if let StmtKind::Fn {
                name,
                parameters,
                ..
            } = &stmt.kind
            {
                if self.host_functions.contains_key(name) {
                    return Err(EngineError::compile(
                        format!("Function '{name}' conflicts with registered host function"),
                        stmt.span.line,
                        stmt.span.column,
                    ));
                }
                self.functions
                    .declare(name, parameters.clone())
                    .map_err(|e| EngineError::compile(e, stmt.span.line, stmt.span.column))?;
            }
        }

        // Pass 2: Compile function bodies (jumped over by main flow)
        let mut fn_stmts = Vec::new();
        let mut main_stmts = Vec::new();

        for stmt in statements {
            match &stmt.kind {
                StmtKind::Fn { .. } => fn_stmts.push(stmt),
                _ => main_stmts.push(stmt),
            }
        }

        let jump_over_functions = if !fn_stmts.is_empty() {
            Some(self.builder.emit(Op::Jump(usize::MAX), 1))
        } else {
            None
        };

        for stmt in fn_stmts {
            self.compile_fn_decl(stmt)?;
        }

        if let Some(jump_op) = jump_over_functions {
            let main_ip = self.builder.current_pos();
            self.builder.patch_jump(jump_op, main_ip);
        }

        // Pass 3: Compile main statements
        // We track the last expression statement to ensure its result is left on stack
        let num_main = main_stmts.len();
        for (i, stmt) in main_stmts.iter().enumerate() {
            let is_last = i == num_main - 1;
            self.compile_stmt(stmt, is_last)?;
        }

        let chunk = Arc::new(self.builder.build());
        let globals = self.symbols.globals();

        Ok(CompiledProgram {
            chunk,
            globals,
            uses_host_functions: self.uses_host_functions,
        })
    }

    fn compile_fn_decl(&mut self, stmt: &Stmt) -> Result<(), EngineError> {
        if let StmtKind::Fn {
            name,
            parameters,
            body,
        } = &stmt.kind
        {
            let name_idx = self.builder.add_name(name);
            let entry_ip = self.builder.current_pos();
            self.builder.add_sub_entry(name_idx, entry_ip);

            let old_symbols = std::mem::replace(&mut self.symbols, SymbolTable::new());
            let old_in_fn = self.in_function;
            self.in_function = true;

            // FuseVM Call pushes arguments left to right, so popping them inside
            // callee arrives in reverse order.
            for param in parameters.iter().rev() {
                let slot = self
                    .symbols
                    .declare(param)
                    .map_err(|e| EngineError::compile(e, stmt.span.line, stmt.span.column))?;
                self.builder.emit(Op::SetSlot(slot), stmt.span.line as u32);
            }

            // Compile body
            for body_stmt in body {
                self.compile_stmt(body_stmt, false)?;
            }

            // Implicit return undefined if control falls off
            self.builder.emit(Op::LoadUndef, stmt.span.line as u32);
            self.builder.emit(Op::ReturnValue, stmt.span.line as u32);

            self.symbols = old_symbols;
            self.in_function = old_in_fn;
        }
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), EngineError> {
        let line = stmt.span.line as u32;

        match &stmt.kind {
            StmtKind::Let { name, value } => {
                let is_global = self.symbols.is_global_scope();
                let slot = self
                    .symbols
                    .declare(name)
                    .map_err(|e| EngineError::compile(e, stmt.span.line, stmt.span.column))?;

                if is_global {
                    // If slot already contains a non-undef value (e.g. preloaded global), do not overwrite with default initializer
                    self.builder.emit(Op::GetSlot(slot), line);
                    self.builder.emit(Op::LoadUndef, line);
                    self.builder.emit(Op::NumEq, line);
                    let skip_init_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), line);

                    self.compile_expr(value)?;
                    self.builder.emit(Op::SetSlot(slot), line);

                    let skip_target = self.builder.current_pos();
                    self.builder.patch_jump(skip_init_jump, skip_target);
                } else {
                    self.compile_expr(value)?;
                    self.builder.emit(Op::SetSlot(slot), line);
                }

                if is_last && !self.in_function {
                    // Let has no expression result, push undef if it's the last statement
                    self.builder.emit(Op::LoadUndef, line);
                }
            }

            StmtKind::Assign { name, value } => {
                let slot = self
                    .symbols
                    .resolve(name)
                    .map_err(|e| EngineError::compile(e, stmt.span.line, stmt.span.column))?;
                self.compile_expr(value)?;
                self.builder.emit(Op::SetSlot(slot), line);
                if is_last && !self.in_function {
                    self.builder.emit(Op::LoadUndef, line);
                }
            }

            StmtKind::Print(expr) => {
                self.compile_expr(expr)?;
                self.builder.emit(Op::PrintLn(1), line);
                if is_last && !self.in_function {
                    self.builder.emit(Op::LoadUndef, line);
                }
            }

            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.compile_expr(condition)?;
                let else_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), line);

                self.compile_stmt(then_branch, false)?;

                if let Some(else_branch) = else_branch {
                    let end_jump = self.builder.emit(Op::Jump(usize::MAX), line);
                    let else_start = self.builder.current_pos();
                    self.builder.patch_jump(else_jump, else_start);

                    self.compile_stmt(else_branch, false)?;

                    let end_pos = self.builder.current_pos();
                    self.builder.patch_jump(end_jump, end_pos);
                } else {
                    let end_pos = self.builder.current_pos();
                    self.builder.patch_jump(else_jump, end_pos);
                }

                if is_last && !self.in_function {
                    self.builder.emit(Op::LoadUndef, line);
                }
            }

            StmtKind::While { condition, body } => {
                let loop_start = self.builder.current_pos();
                self.compile_expr(condition)?;
                let exit_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), line);

                self.compile_stmt(body, false)?;

                self.builder.emit(Op::Jump(loop_start), line);

                let exit_pos = self.builder.current_pos();
                self.builder.patch_jump(exit_jump, exit_pos);

                if is_last && !self.in_function {
                    self.builder.emit(Op::LoadUndef, line);
                }
            }

            StmtKind::Return(expr_opt) => {
                if !self.in_function {
                    return Err(EngineError::compile(
                        "Return statement outside function",
                        stmt.span.line,
                        stmt.span.column,
                    ));
                }
                if let Some(expr) = expr_opt {
                    self.compile_expr(expr)?;
                    self.builder.emit(Op::ReturnValue, line);
                } else {
                    self.builder.emit(Op::LoadUndef, line);
                    self.builder.emit(Op::ReturnValue, line);
                }
            }

            StmtKind::Block(stmts) => {
                self.symbols.enter_scope();
                for (i, s) in stmts.iter().enumerate() {
                    let block_last = is_last && (i == stmts.len() - 1);
                    self.compile_stmt(s, block_last)?;
                }
                self.symbols.exit_scope();
            }

            StmtKind::Expression(expr) => {
                self.compile_expr(expr)?;
                if !(is_last && !self.in_function) {
                    // Discard intermediate expression results
                    self.builder.emit(Op::Pop, line);
                }
            }

            StmtKind::Fn { .. } => {}
        }

        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), EngineError> {
        let line = expr.span.line as u32;

        match &expr.kind {
            ExprKind::Int(v) => {
                self.builder.emit(Op::LoadInt(*v), line);
            }

            ExprKind::Float(v) => {
                self.builder.emit(Op::LoadFloat(*v), line);
            }

            ExprKind::String(v) => {
                let c = self.builder.add_constant(Value::str(v.clone()));
                self.builder.emit(Op::LoadConst(c), line);
            }

            ExprKind::Bool(b) => {
                if *b {
                    self.builder.emit(Op::LoadTrue, line);
                } else {
                    self.builder.emit(Op::LoadFalse, line);
                }
            }

            ExprKind::Variable(name) => {
                let slot = self
                    .symbols
                    .resolve(name)
                    .map_err(|e| EngineError::compile(e, expr.span.line, expr.span.column))?;
                self.builder.emit(Op::GetSlot(slot), line);
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

                match operator {
                    BinaryOp::Add => self.builder.emit(Op::Add, line),
                    BinaryOp::Subtract => self.builder.emit(Op::Sub, line),
                    BinaryOp::Multiply => self.builder.emit(Op::Mul, line),
                    BinaryOp::Divide => self.builder.emit(Op::Div, line),
                    BinaryOp::Equal => self.builder.emit(Op::NumEq, line),
                    BinaryOp::NotEqual => self.builder.emit(Op::NumNe, line),
                    BinaryOp::Less => self.builder.emit(Op::NumLt, line),
                    BinaryOp::LessEqual => self.builder.emit(Op::NumLe, line),
                    BinaryOp::Greater => self.builder.emit(Op::NumGt, line),
                    BinaryOp::GreaterEqual => self.builder.emit(Op::NumGe, line),
                };
            }

            ExprKind::Call { callee, arguments } => {
                let callee_name = match &callee.kind {
                    ExprKind::Variable(name) => name,
                    _ => {
                        return Err(EngineError::compile(
                            "Only named functions can be called",
                            callee.span.line,
                            callee.span.column,
                        ));
                    }
                };

                let argc = arguments.len();
                if argc > 255 {
                    return Err(EngineError::compile(
                        "Too many arguments in function call",
                        callee.span.line,
                        callee.span.column,
                    ));
                }

                // Check if user function
                if let Some(sig) = self.functions.resolve(callee_name).cloned() {
                    if sig.arity != argc {
                        return Err(EngineError::compile(
                            format!(
                                "Function '{callee_name}' expects {} arguments, got {}",
                                sig.arity, argc
                            ),
                            callee.span.line,
                            callee.span.column,
                        ));
                    }

                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }

                    let name_idx = self.builder.add_name(callee_name);
                    self.builder.emit(Op::Call(name_idx, argc as u8), line);
                } else if let Some(&host_id) = self.host_functions.get(callee_name) {
                    self.uses_host_functions = true;

                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }

                    let ext_id = EXT_HOST_BASE + host_id;
                    self.builder.emit(Op::Extended(ext_id, argc as u8), line);
                } else {
                    return Err(EngineError::compile(
                        format!("Undefined function: '{callee_name}'"),
                        callee.span.line,
                        callee.span.column,
                    ));
                }
            }
        }

        Ok(())
    }
}

pub fn compile(
    statements: &[Stmt],
    host_functions: &HashMap<String, u16>,
) -> Result<CompiledProgram, EngineError> {
    Compiler::new(host_functions).compile(statements)
}
