//! Sync index — maps rendered element ids back to their true source location.
//!
//! Step 1 only populates this; forward/inverse search consumers arrive in
//! Steps 5 and 6.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ast::Pos;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    pub element_id: String,
    pub file: PathBuf,
    pub start: Pos,
    pub end: Pos,
    pub label: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncIndex {
    pub entries: Vec<SyncEntry>,
    by_label: HashMap<String, usize>,
}

impl SyncIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(
        &mut self,
        element_id: impl Into<String>,
        file: PathBuf,
        start: Pos,
        end: Pos,
        label: Option<String>,
    ) {
        let element_id = element_id.into();
        let idx = self.entries.len();
        if let Some(ref l) = label {
            self.by_label.insert(l.clone(), idx);
        }
        self.entries.push(SyncEntry {
            element_id,
            file,
            start,
            end,
            label,
        });
    }

    pub fn lookup_by_label(&self, label: &str) -> Option<&SyncEntry> {
        self.by_label.get(label).and_then(|i| self.entries.get(*i))
    }
}
