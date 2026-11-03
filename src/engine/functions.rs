use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FunctionSignature {
    pub name: String,
    pub arity: usize,
    pub parameters: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FunctionTable {
    functions: HashMap<String, FunctionSignature>,
}

impl FunctionTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn declare(
        &mut self,
        name: &str,
        parameters: Vec<String>,
    ) -> Result<(), String> {
        if self.functions.contains_key(name) {
            return Err(format!("Function already declared: {name}"));
        }

        self.functions.insert(
            name.to_string(),
            FunctionSignature {
                name: name.to_string(),
                arity: parameters.len(),
                parameters,
            },
        );

        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Option<&FunctionSignature> {
        self.functions.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.functions.contains_key(name)
    }
}
