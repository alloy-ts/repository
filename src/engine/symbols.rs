//! Symbol table for variable slot resolution in Engine.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SymbolTable {
    scopes: Vec<HashMap<String, u16>>,
    next_slot: u16,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
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
        if let Some(current) = self.scopes.last_mut() {
            if current.contains_key(name) {
                return Err(format!("Variable '{name}' already declared in current scope"));
            }
            let slot = self.next_slot;
            self.next_slot = self
                .next_slot
                .checked_add(1)
                .ok_or_else(|| "Exceeded maximum variable limit (u16::MAX)".to_string())?;
            current.insert(name.to_string(), slot);
            Ok(slot)
        } else {
            Err("No active scope in symbol table".to_string())
        }
    }

    pub fn resolve(&self, name: &str) -> Result<u16, String> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.get(name) {
                return Ok(slot);
            }
        }
        Err(format!("Undefined variable '{name}'"))
    }

    pub fn get_root_declarations(&self) -> HashMap<String, u16> {
        self.scopes.first().cloned().unwrap_or_default()
    }

    pub fn total_slots(&self) -> u16 {
        self.next_slot
    }
}
