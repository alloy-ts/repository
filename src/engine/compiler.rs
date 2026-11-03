use fusevm::{ChunkBuilder, Op, Value};
use std::collections::HashMap;

use crate::engine::ast::{BinaryOp, Expr, Stmt};
use crate::engine::functions::FunctionTable;
use crate::engine::symbols::SymbolTable;

pub const EXT_HOST_CALL: u16 = 100;
pub const EXT_PRINT: u16 = 101;

pub struct Compiler {
    builder: ChunkBuilder,
    symbols: SymbolTable,
    functions: FunctionTable,
    host_functions: HashMap<String, u16>,
    in_function: bool,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            builder: ChunkBuilder::new(),
            symbols: SymbolTable::new(),
            functions: FunctionTable::new(),
            host_functions: HashMap::new(),
            in_function: false,
        }
    }

    pub fn with_host_functions(host_functions: HashMap<String, u16>) -> Self {
        Self {
            builder: ChunkBuilder::new(),
            symbols: SymbolTable::new(),
            functions: FunctionTable::new(),
            host_functions,
            in_function: false,
        }
    }

    pub fn compile(mut self, statements: &[Stmt]) -> Result<ChunkBuilder, String> {
        // Pass 1: Register function declarations
        for stmt in statements {
            if let Stmt::Function {
                name, parameters, ..
            } = stmt
            {
                let name_idx = self.builder.add_name(name);
                self.functions
                    .declare(name.clone(), name_idx, parameters.clone())?;
            }
        }

        // Emit main program jumping past function definitions if needed,
        // or compile functions as sub-routines with add_sub_entry.
        let mut function_stmts = Vec::new();
        let mut main_stmts = Vec::new();

        for stmt in statements {
            match stmt {
                Stmt::Function { .. } => function_stmts.push(stmt),
                _ => main_stmts.push(stmt),
            }
        }

        // Jump over function bodies to start of main script
        let main_jump = if !function_stmts.is_empty() {
            Some(self.builder.emit(Op::Jump(usize::MAX), 1))
        } else {
            None
        };

        // Compile functions
        for stmt in function_stmts {
            if let Stmt::Function {
                name,
                parameters,
                body,
            } = stmt
            {
                let func_info = self.functions.resolve(name)?.clone();
                let entry_ip = self.builder.current_pos();
                self.builder.add_sub_entry(func_info.name_idx, entry_ip);

                // Compile function body in its own isolated symbol scope
                let saved_symbols = std::mem::replace(&mut self.symbols, SymbolTable::new());
                let saved_in_function = self.in_function;
                self.in_function = true;

                // When Op::Call(name_idx, argc) runs, the arguments are at the top of the stack.
                // In fusevm, arguments are pushed in left-to-right order:
                // arg0 is pushed, then arg1 is pushed, etc.
                // So top of stack is the LAST argument (parameters[n-1]), then parameters[n-2], ..., down to parameters[0].
                // We allocate slots 0..parameters.len() for parameters.
                // To pop arguments into slots, we pop from last to first!
                for param in parameters {
                    self.symbols.declare(param)?;
                }

                for (idx, _param) in parameters.iter().enumerate().rev() {
                    self.builder.emit(Op::SetSlot(idx as u16), 1);
                }

                let mut returned = false;
                for s in body {
                    self.compile_stmt(s)?;
                    if matches!(s, Stmt::Return(_)) {
                        returned = true;
                    }
                }

                // If not explicitly returned, return Undef
                if !returned {
                    self.builder.emit(Op::LoadUndef, 1);
                    self.builder.emit(Op::ReturnValue, 1);
                }

                self.symbols = saved_symbols;
                self.in_function = saved_in_function;
            }
        }

        if let Some(jump) = main_jump {
            let main_start = self.builder.current_pos();
            self.builder.patch_jump(jump, main_start);
        }

        // Compile main script statements
        for stmt in main_stmts {
            self.compile_stmt(stmt)?;
        }

        Ok(self.builder)
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, value } => {
                let slot = self.symbols.declare(name)?;
                self.compile_expr(value)?;
                self.builder.emit(Op::SetSlot(slot), 1);
            }
            Stmt::Assign { name, value } => {
                let slot = self.symbols.resolve(name)?;
                self.compile_expr(value)?;
                self.builder.emit(Op::SetSlot(slot), 1);
            }
            Stmt::Print(expr) => {
                self.compile_expr(expr)?;
                // Op::PrintLn(n) prints n items from stack and adds newline
                self.builder.emit(Op::PrintLn(1), 1);
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.compile_if(condition, then_branch, else_branch)?;
            }
            Stmt::While { condition, body } => {
                self.compile_while(condition, body)?;
            }
            Stmt::Block(stmts) => {
                self.symbols.enter_scope();
                for s in stmts {
                    self.compile_stmt(s)?;
                }
                self.symbols.exit_scope();
            }
            Stmt::Return(val) => {
                if let Some(expr) = val {
                    self.compile_expr(expr)?;
                } else {
                    self.builder.emit(Op::LoadUndef, 1);
                }
                self.builder.emit(Op::ReturnValue, 1);
            }
            Stmt::Expression(expr) => {
                self.compile_expr(expr)?;
                // Pop expression statement results except for the very last statement if outside function
                self.builder.emit(Op::Pop, 1);
            }
            Stmt::Function { .. } => {
                // Handled in pass 1
            }
        }
        Ok(())
    }

    fn compile_if(
        &mut self,
        condition: &Expr,
        then_branch: &[Stmt],
        else_branch: &[Stmt],
    ) -> Result<(), String> {
        self.compile_expr(condition)?;
        let else_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), 1);

        self.symbols.enter_scope();
        for s in then_branch {
            self.compile_stmt(s)?;
        }
        self.symbols.exit_scope();

        if !else_branch.is_empty() {
            let end_jump = self.builder.emit(Op::Jump(usize::MAX), 1);
            let else_start = self.builder.current_pos();
            self.builder.patch_jump(else_jump, else_start);

            self.symbols.enter_scope();
            for s in else_branch {
                self.compile_stmt(s)?;
            }
            self.symbols.exit_scope();

            let end = self.builder.current_pos();
            self.builder.patch_jump(end_jump, end);
        } else {
            let end = self.builder.current_pos();
            self.builder.patch_jump(else_jump, end);
        }
        Ok(())
    }

    fn compile_while(&mut self, condition: &Expr, body: &[Stmt]) -> Result<(), String> {
        let loop_start = self.builder.current_pos();
        self.compile_expr(condition)?;
        let exit_jump = self.builder.emit(Op::JumpIfFalse(usize::MAX), 1);

        self.symbols.enter_scope();
        for s in body {
            self.compile_stmt(s)?;
        }
        self.symbols.exit_scope();

        self.builder.emit(Op::Jump(loop_start), 1);
        let exit = self.builder.current_pos();
        self.builder.patch_jump(exit_jump, exit);

        Ok(())
    }

    pub fn compile_expr(&mut self, expr: &Expr) -> Result<(), String> {
        match expr {
            Expr::Number(n) => {
                self.builder.emit(Op::LoadInt(*n), 1);
            }
            Expr::Float(f) => {
                self.builder.emit(Op::LoadFloat(*f), 1);
            }
            Expr::String(s) => {
                let const_idx = self.builder.add_constant(Value::str(s.clone()));
                self.builder.emit(Op::LoadConst(const_idx), 1);
            }
            Expr::Bool(b) => {
                if *b {
                    self.builder.emit(Op::LoadTrue, 1);
                } else {
                    self.builder.emit(Op::LoadFalse, 1);
                }
            }
            Expr::Variable(name) => {
                let slot = self.symbols.resolve(name)?;
                self.builder.emit(Op::GetSlot(slot), 1);
            }
            Expr::Call { name, arguments } => {
                // Check if it's a Nova function
                if let Ok(func_info) = self.functions.resolve(name) {
                    if arguments.len() != func_info.parameters.len() {
                        return Err(format!(
                            "Function '{name}' expects {} arguments, got {}",
                            func_info.parameters.len(),
                            arguments.len()
                        ));
                    }
                    let name_idx = func_info.name_idx;
                    let argc = arguments.len() as u8;
                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }
                    self.builder.emit(Op::Call(name_idx, argc), 1);
                } else if let Some(&fn_id) = self.host_functions.get(name) {
                    // Host function call via Op::Extended
                    let argc = arguments.len() as u8;
                    for arg in arguments {
                        self.compile_expr(arg)?;
                    }
                    self.builder.emit(Op::Extended(fn_id, argc), 1);
                } else {
                    return Err(format!("Undefined function: '{name}'"));
                }
            }
            Expr::Binary {
                left,
                operator,
                right,
            } => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;

                match operator {
                    BinaryOp::Add => {
                        // In fusevm, Op::Add works for numbers, but Op::Concat works for strings
                        // FuseVM Op::Add on numbers is Op::Add
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
                    BinaryOp::Modulo => {
                        self.builder.emit(Op::Mod, 1);
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
        }
        Ok(())
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}
