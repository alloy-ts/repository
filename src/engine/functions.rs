use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub name: String,
    pub name_idx: u16,
    pub parameters: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct FunctionTable {
    functions: HashMap<String, FunctionInfo>,
}

impl FunctionTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn declare(
        &mut self,
        name: String,
        name_idx: u16,
        parameters: Vec<String>,
    ) -> Result<(), String> {
        if self.functions.contains_key(&name) {
            return Err(format!("Function already declared: {name}"));
        }

        self.functions.insert(
            name.clone(),
            FunctionInfo {
                name,
                name_idx,
                parameters,
            },
        );

        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Result<&FunctionInfo, String> {
        self.functions
            .get(name)
            .ok_or_else(|| format!("Undefined function: {name}"))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.functions.contains_key(name)
    }
}
