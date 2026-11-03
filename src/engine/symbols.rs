use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Scope {
    pub slots: HashMap<String, u16>,
}

#[derive(Debug, Clone)]
pub struct SymbolTable {
    scopes: Vec<Scope>,
    next_slot: u16,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope {
                slots: HashMap::new(),
            }],
            next_slot: 0,
        }
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(Scope {
            slots: HashMap::new(),
        });
    }

    pub fn exit_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    pub fn declare(&mut self, name: &str) -> Result<u16, String> {
        if let Some(current_scope) = self.scopes.last_mut() {
            if current_scope.slots.contains_key(name) {
                return Err(format!("Variable already declared in current scope: {name}"));
            }
            let slot = self.next_slot;
            self.next_slot = self
                .next_slot
                .checked_add(1)
                .ok_or_else(|| "Too many variables: slot limit exceeded".to_string())?;
            current_scope.slots.insert(name.to_string(), slot);
            Ok(slot)
        } else {
            Err("No active scope".to_string())
        }
    }

    pub fn resolve(&self, name: &str) -> Result<u16, String> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.slots.get(name) {
                return Ok(slot);
            }
        }
        Err(format!("Undefined variable: {name}"))
    }

    pub fn contains_in_current_scope(&self, name: &str) -> bool {
        self.scopes
            .last()
            .map(|s| s.slots.contains_key(name))
            .unwrap_or(false)
    }

    pub fn total_slots(&self) -> u16 {
        self.next_slot
    }
}
