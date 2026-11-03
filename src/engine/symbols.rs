use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Scope {
    slots: HashMap<String, u16>,
}

#[derive(Debug, Clone)]
pub struct SymbolTable {
    scopes: Vec<Scope>,
    next_slot: u16,
    max_slots: u16,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope {
                slots: HashMap::new(),
            }],
            next_slot: 0,
            max_slots: 0,
        }
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(Scope {
            slots: HashMap::new(),
        });
    }

    pub fn exit_scope(&mut self) {
        if self.scopes.len() > 1 {
            let exiting = self.scopes.pop().unwrap();
            let count = exiting.slots.len() as u16;
            self.next_slot = self.next_slot.saturating_sub(count);
        }
    }

    pub fn is_global_scope(&self) -> bool {
        self.scopes.len() == 1
    }

    pub fn declare(&mut self, name: &str) -> Result<u16, String> {
        let current_scope = self.scopes.last_mut().ok_or("No active scope")?;
        if current_scope.slots.contains_key(name) {
            return Err(format!("Variable already declared in current scope: {name}"));
        }

        let slot = self.next_slot;
        self.next_slot = self
            .next_slot
            .checked_add(1)
            .ok_or("Too many local variable slots")?;
        if self.next_slot > self.max_slots {
            self.max_slots = self.next_slot;
        }

        current_scope.slots.insert(name.to_string(), slot);
        Ok(slot)
    }

    pub fn resolve(&self, name: &str) -> Result<u16, String> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.slots.get(name) {
                return Ok(slot);
            }
        }
        Err(format!("Undefined variable: {name}"))
    }

    pub fn globals(&self) -> HashMap<String, u16> {
        self.scopes
            .first()
            .map(|s| s.slots.clone())
            .unwrap_or_default()
    }

    pub fn max_slots(&self) -> u16 {
        self.max_slots
    }
}
