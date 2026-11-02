use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SymbolTable {
    scopes: Vec<HashMap<String, u16>>,
    next_slot: u16,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            next_slot: 0,
        }
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn exit_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    pub fn declare(&mut self, name: &str) -> Result<u16, String> {
        let current_scope = self.scopes.last_mut().expect("No active scope");
        if current_scope.contains_key(name) {
            return Err(format!("Variable already declared in current scope: {name}"));
        }

        let slot = self.next_slot;
        self.next_slot = self
            .next_slot
            .checked_add(1)
            .ok_or_else(|| "Too many variables (slots exhausted)".to_string())?;

        current_scope.insert(name.to_string(), slot);
        Ok(slot)
    }

    pub fn resolve(&self, name: &str) -> Result<u16, String> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.get(name) {
                return Ok(slot);
            }
        }
        Err(format!("Undefined variable: {name}"))
    }

    pub fn is_declared_in_current_scope(&self, name: &str) -> bool {
        self.scopes
            .last()
            .map(|s| s.contains_key(name))
            .unwrap_or(false)
    }

    pub fn next_slot(&self) -> u16 {
        self.next_slot
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}
