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
    /// A block-level leaf (e.g. a section heading): included in a selection
    /// range like a Leaf, but excluded from the single-point cursor lookup so a
    /// cursor on it doesn't flash the whole line — only inline leaves flash.
    Block,
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

/// Per-row source spans for a multi-row display-math block (align/gather/…).
/// Lets an editor selection highlight the individual rows it covers rather than
/// the whole block. `rows[i]` is the (start_line, end_line) of the i-th rendered
/// table row, so the client maps overlapping indices to the SVG `mtr` groups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathRowsEntry {
    pub element_id: String,
    pub file: PathBuf,
    pub rows: Vec<(u32, u32)>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncIndex {
    pub entries: Vec<SyncEntry>,
    #[serde(default)]
    pub math_rows: Vec<MathRowsEntry>,
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

    /// Record the per-row source spans of a multi-row math block.
    pub fn record_math_rows(
        &mut self,
        element_id: impl Into<String>,
        file: PathBuf,
        rows: Vec<(u32, u32)>,
    ) {
        self.math_rows.push(MathRowsEntry {
            element_id: element_id.into(),
            file,
            rows,
        });
    }

    /// For each multi-row math block in `file` with at least one row whose line
    /// span overlaps `[start_line, end_line]`, return its element id, total row
    /// count, and the overlapping row indices (line-based; the client maps the
    /// indices to the rendered `mtr` rows and falls back to a whole-block
    /// highlight if the count doesn't match).
    pub fn math_rows_in_range(
        &self,
        file: &Path,
        start_line: u32,
        end_line: u32,
    ) -> Vec<(String, usize, Vec<usize>)> {
        let mut out = Vec::new();
        for m in &self.math_rows {
            if !same_path(&m.file, file) {
                continue;
            }
            let hits: Vec<usize> = m
                .rows
                .iter()
                .enumerate()
                .filter(|(_, &(rs, re))| rs <= end_line && re >= start_line)
                .map(|(i, _)| i)
                .collect();
            if !hits.is_empty() {
                out.push((m.element_id.clone(), m.rows.len(), hits));
            }
        }
        out
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

    /// All leaf element ids in `file` whose source span overlaps the inclusive
    /// range `start..=end`. The range generalization of
    /// [`Self::lookup_leaf_by_source_position`] used for highlighting an editor
    /// selection. Leaf-only (so a multi-word selection doesn't also flag the
    /// enclosing theorem/proof container), and — unlike the point lookup — it
    /// does NOT snap to the nearest entry: a selection over un-indexed source
    /// simply yields fewer (or no) ids.
    pub fn leaves_in_range(
        &self,
        file: &Path,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    ) -> Vec<String> {
        let start = Pos {
            line: start_line,
            col: start_col,
            byte: 0,
        };
        let end = Pos {
            line: end_line,
            col: end_col,
            byte: 0,
        };
        self.entries
            .iter()
            // Leaf and Block both participate in a selection range; only
            // Container (theorem/proof wrappers) is excluded.
            .filter(|e| e.kind != SyncKind::Container && same_path(&e.file, file))
            // Overlap: entry.start <= sel_end && entry.end >= sel_start.
            .filter(|e| pos_before_or_equal(e.start, end) && pos_after_or_equal(e.end, start))
            .map(|e| e.element_id.clone())
            .collect()
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

    #[test]
    fn leaves_in_range_returns_overlapping_leaves_only() {
        let file = PathBuf::from("/tmp/main.tex");
        let other = PathBuf::from("/tmp/other.tex");
        let mut sync = SyncIndex::new();
        // A container spanning the whole region (must be excluded).
        sync.record_with_kind(
            "proof-1",
            file.clone(),
            pos(10, 1),
            pos(20, 1),
            None,
            SyncKind::Container,
        );
        sync.record("w-a", file.clone(), pos(11, 1), pos(11, 5), None);
        sync.record("w-b", file.clone(), pos(12, 1), pos(12, 5), None);
        sync.record("w-c", file.clone(), pos(18, 1), pos(18, 5), None);
        // Same source coordinates but a different file — must be excluded.
        sync.record("other-w", other, pos(11, 1), pos(11, 5), None);

        // Selection covering lines 11..12 hits w-a and w-b, not the container,
        // not the far w-c, not the other file.
        let ids = sync.leaves_in_range(&file, 11, 1, 12, 3);
        assert_eq!(ids, vec!["w-a".to_string(), "w-b".to_string()]);

        // A boundary-touching selection still overlaps (inclusive).
        assert_eq!(sync.leaves_in_range(&file, 11, 5, 11, 5), vec!["w-a"]);

        // A selection over un-indexed source snaps to nothing (no fallback).
        assert!(sync.leaves_in_range(&file, 30, 1, 31, 1).is_empty());
    }

    #[test]
    fn math_rows_in_range_returns_overlapping_row_indices() {
        let file = PathBuf::from("/tmp/main.tex");
        let mut sync = SyncIndex::new();
        // A 3-row align with rows on source lines 4, 5, 6.
        sync.record_math_rows("dm-1", file.clone(), vec![(4, 4), (5, 5), (6, 6)]);

        // Selecting lines 4..5 hits rows 0 and 1 (not row 2); count is reported.
        assert_eq!(
            sync.math_rows_in_range(&file, 4, 5),
            vec![("dm-1".to_string(), 3, vec![0, 1])]
        );
        // A single-line selection on row 2.
        assert_eq!(
            sync.math_rows_in_range(&file, 6, 6),
            vec![("dm-1".to_string(), 3, vec![2])]
        );
        // No overlap → nothing.
        assert!(sync.math_rows_in_range(&file, 1, 2).is_empty());
        // Wrong file → nothing.
        assert!(sync
            .math_rows_in_range(Path::new("/tmp/other.tex"), 4, 6)
            .is_empty());
    }
}
