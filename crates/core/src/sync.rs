//! Sync index — maps rendered element ids back to their true source location.
//!
//! Step 1 only populates this; forward/inverse search consumers arrive in
//! Steps 5 and 6.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ast::Pos;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncKind {
    #[default]
    Leaf,
    Container,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    pub element_id: String,
    pub file: PathBuf,
    pub start: Pos,
    pub end: Pos,
    pub label: Option<String>,
    #[serde(default)]
    pub kind: SyncKind,
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
        self.record_with_kind(element_id, file, start, end, label, SyncKind::Leaf);
    }

    pub fn record_with_kind(
        &mut self,
        element_id: impl Into<String>,
        file: PathBuf,
        start: Pos,
        end: Pos,
        label: Option<String>,
        kind: SyncKind,
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
            kind,
        });
    }

    pub fn lookup_by_label(&self, label: &str) -> Option<&SyncEntry> {
        self.by_label.get(label).and_then(|i| self.entries.get(*i))
    }

    pub fn lookup_by_source_position(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Option<&SyncEntry> {
        self.lookup_by_source_position_filtered(file, line, col, None, true)
    }

    pub fn lookup_leaf_by_source_position(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Option<&SyncEntry> {
        self.lookup_by_source_position_filtered(file, line, col, Some(SyncKind::Leaf), false)
    }

    fn lookup_by_source_position_filtered(
        &self,
        file: &Path,
        line: u32,
        col: u32,
        kind: Option<SyncKind>,
        allow_cross_line_fallback: bool,
    ) -> Option<&SyncEntry> {
        let pos = Pos { line, col, byte: 0 };
        let mut containing: Option<(usize, u32)> = None;
        let mut before: Option<(usize, u32, u32)> = None;
        let mut after: Option<(usize, u32, u32)> = None;

        for (idx, entry) in self.entries.iter().enumerate() {
            if kind.is_some_and(|expected| entry.kind != expected) {
                continue;
            }
            if !same_path(&entry.file, file) {
                continue;
            }
            if contains_pos(entry.start, entry.end, pos) {
                let size = span_score(entry.start, entry.end);
                if containing.is_none_or(|(_, best)| size < best) {
                    containing = Some((idx, size));
                }
                continue;
            }
            if pos_after_or_equal(pos, entry.start) {
                let line_delta = pos.line.saturating_sub(entry.start.line);
                if !allow_cross_line_fallback && line_delta != 0 {
                    continue;
                }
                let col_delta = pos.col.saturating_sub(entry.start.col);
                if before.is_none_or(|(_, best_line, best_col)| {
                    line_delta < best_line || (line_delta == best_line && col_delta < best_col)
                }) {
                    before = Some((idx, line_delta, col_delta));
                }
            } else {
                let line_delta = entry.start.line.saturating_sub(pos.line);
                if !allow_cross_line_fallback && line_delta != 0 {
                    continue;
                }
                let col_delta = entry.start.col.saturating_sub(pos.col);
                if after.is_none_or(|(_, best_line, best_col)| {
                    line_delta < best_line || (line_delta == best_line && col_delta < best_col)
                }) {
                    after = Some((idx, line_delta, col_delta));
                }
            }
        }

        containing
            .map(|(idx, _)| idx)
            .or_else(|| before.map(|(idx, _, _)| idx))
            .or_else(|| after.map(|(idx, _, _)| idx))
            .and_then(|idx| self.entries.get(idx))
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    a.to_string_lossy() == b.to_string_lossy()
}

fn contains_pos(start: Pos, end: Pos, pos: Pos) -> bool {
    pos_after_or_equal(pos, start) && pos_before_or_equal(pos, end)
}

fn pos_after_or_equal(pos: Pos, start: Pos) -> bool {
    pos.line > start.line || (pos.line == start.line && pos.col >= start.col)
}

fn pos_before_or_equal(pos: Pos, end: Pos) -> bool {
    pos.line < end.line || (pos.line == end.line && pos.col <= end.col)
}

fn span_score(start: Pos, end: Pos) -> u32 {
    let lines = end.line.saturating_sub(start.line);
    let cols = if lines == 0 {
        end.col.saturating_sub(start.col)
    } else {
        end.col
    };
    lines.saturating_mul(100_000).saturating_add(cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, col: u32) -> Pos {
        Pos { line, col, byte: 0 }
    }

    #[test]
    fn lookup_by_source_prefers_smallest_containing_span() {
        let file = PathBuf::from("/tmp/main.tex");
        let mut sync = SyncIndex::new();
        sync.record("blk-1", file.clone(), pos(10, 1), pos(14, 1), None);
        sync.record("eq-main", file.clone(), pos(12, 3), pos(12, 20), None);

        let entry = sync.lookup_by_source_position(&file, 12, 8).expect("entry");
        assert_eq!(entry.element_id, "eq-main");
    }

    #[test]
    fn lookup_by_source_falls_back_to_nearest_previous_entry() {
        let file = PathBuf::from("/tmp/main.tex");
        let mut sync = SyncIndex::new();
        sync.record("blk-1", file.clone(), pos(10, 1), pos(10, 8), None);
        sync.record("blk-2", file.clone(), pos(20, 1), pos(20, 8), None);

        let entry = sync.lookup_by_source_position(&file, 14, 1).expect("entry");
        assert_eq!(entry.element_id, "blk-1");
    }

    #[test]
    fn leaf_lookup_ignores_containers_and_cross_line_fallback() {
        let file = PathBuf::from("/tmp/main.tex");
        let mut sync = SyncIndex::new();
        sync.record_with_kind(
            "proof-1",
            file.clone(),
            pos(10, 1),
            pos(20, 1),
            None,
            SyncKind::Container,
        );
        sync.record("srcw-1", file.clone(), pos(11, 3), pos(11, 8), None);

        let in_word = sync
            .lookup_leaf_by_source_position(&file, 11, 5)
            .expect("leaf word entry");
        assert_eq!(in_word.element_id, "srcw-1");
        assert!(sync.lookup_leaf_by_source_position(&file, 15, 1).is_none());
    }
}
