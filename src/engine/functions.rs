//! Function registry and resolution for Engine.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub name: String,
    pub entry: usize,
    pub parameters: Vec<String>,
}

#[derive(Default, Debug, Clone)]
pub struct FunctionTable {
    functions: HashMap<String, FunctionInfo>,
}

impl FunctionTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn declare(&mut self, name: String, entry: usize, parameters: Vec<String>) -> Result<(), String> {
        if self.functions.contains_key(&name) {
            return Err(format!("Function '{name}' already declared"));
        }

        self.functions.insert(
            name.clone(),
            FunctionInfo {
                name,
                entry,
                parameters,
            },
        );

        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Result<&FunctionInfo, String> {
        self.functions
            .get(name)
            .ok_or_else(|| format!("Undefined function '{name}'"))
    }
}
