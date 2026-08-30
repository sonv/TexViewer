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

/// One rendered row of a multi-row math block: its inclusive source line range
/// plus the byte column of the row's first non-whitespace character.
/// `start_col == 0` means "unknown": the row starts on the `\begin` line, whose
/// file column the renderer can't see from the body slice alone — consumers
/// fall back to the block anchor's column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MathRow {
    pub start_line: u32,
    pub end_line: u32,
    pub start_col: u32,
}

/// Per-row source spans for a multi-row display-math block (align/gather/…).
/// Forward: lets an editor selection highlight the individual rows it covers
/// rather than the whole block. Backward: lets a click on the i-th rendered
/// `mtr` row jump to that row's own source line instead of the `\begin` line.
/// `rows[i]` corresponds to the i-th rendered table row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathRowsEntry {
    pub element_id: String,
    pub file: PathBuf,
    pub rows: Vec<MathRow>,
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
        rows: Vec<MathRow>,
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
                .filter(|(_, r)| r.start_line <= end_line && r.end_line >= start_line)
                .map(|(i, _)| i)
                .collect();
            if !hits.is_empty() {
                out.push((m.element_id.clone(), m.rows.len(), hits));
            }
        }
        out
    }

    /// Backward-search counterpart of `math_rows_in_range`: the source position
    /// of the `row`-th rendered row of block `element_id` in `file`, as
    /// `(line, col)` with a 1-based byte col (col 0 = row starts on the
    /// `\begin` line; the caller keeps the block anchor's column).
    /// `expected_count` is how many rows the client's rendered SVG actually
    /// has — a mismatch means source and render disagree (mid-edit skew), so
    /// return None and let the caller fall back to the block anchor, exactly
    /// like the forward direction falls back to a whole-block highlight.
    pub fn math_row_pos(
        &self,
        element_id: &str,
        file: &Path,
        row: usize,
        expected_count: usize,
    ) -> Option<(u32, u32)> {
        let m = self
            .math_rows
            .iter()
            .find(|m| m.element_id == element_id && same_path(&m.file, file))?;
        if m.rows.len() != expected_count {
            return None;
        }
        let r = m.rows.get(row)?;
        Some((r.start_line, r.start_col))
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

    /// The smallest structural container that actually contains this source
    /// position, without nearest-element fallback. Used to follow otherwise
    /// invisible Markdown syntax such as a code fence or table delimiter row
    /// without snapping unrelated blank/preamble lines into the document.
    pub fn lookup_containing_container_by_source_position(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Option<&SyncEntry> {
        let pos = Pos { line, col, byte: 0 };
        self.entries
            .iter()
            .filter(|entry| {
                entry.kind == SyncKind::Container
                    && same_path(&entry.file, file)
                    && contains_pos(entry.start, entry.end, pos)
            })
            .min_by_key(|entry| span_score(entry.start, entry.end))
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
            // AST spans are half-open while the editor selection is inclusive:
            // entry.start <= sel_end && entry.end > sel_start. Preserve the
            // occasional zero-width anchor by treating its one position as a
            // point range.
            .filter(|e| span_overlaps_inclusive_selection(e.start, e.end, start, end))
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
    if same_position(start, end) {
        same_position(start, pos)
    } else {
        pos_after_or_equal(pos, start) && pos_before(pos, end)
    }
}

fn pos_after_or_equal(pos: Pos, start: Pos) -> bool {
    pos.line > start.line || (pos.line == start.line && pos.col >= start.col)
}

fn pos_before_or_equal(pos: Pos, end: Pos) -> bool {
    pos.line < end.line || (pos.line == end.line && pos.col <= end.col)
}

fn pos_before(pos: Pos, end: Pos) -> bool {
    pos.line < end.line || (pos.line == end.line && pos.col < end.col)
}

fn same_position(a: Pos, b: Pos) -> bool {
    a.line == b.line && a.col == b.col
}

fn span_overlaps_inclusive_selection(
    span_start: Pos,
    span_end: Pos,
    selection_start: Pos,
    selection_end: Pos,
) -> bool {
    if same_position(span_start, span_end) {
        pos_after_or_equal(span_start, selection_start)
            && pos_before_or_equal(span_start, selection_end)
    } else {
        pos_before_or_equal(span_start, selection_end)
            && pos_after_or_equal(span_end, selection_start)
            && !same_position(span_end, selection_start)
    }
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

        // Entry spans are half-open: a selection at the exclusive end does
        // not leak into the preceding leaf.
        assert!(sync.leaves_in_range(&file, 11, 5, 11, 5).is_empty());

        // A selection over un-indexed source snaps to nothing (no fallback).
        assert!(sync.leaves_in_range(&file, 30, 1, 31, 1).is_empty());
    }

    #[test]
    fn adjacent_half_open_spans_choose_only_the_new_leaf_at_a_boundary() {
        let file = PathBuf::from("/tmp/main.md");
        let mut sync = SyncIndex::new();
        sync.record("before", file.clone(), pos(1, 1), pos(1, 8), None);
        sync.record("strong", file.clone(), pos(1, 8), pos(1, 16), None);

        assert_eq!(
            sync.lookup_leaf_by_source_position(&file, 1, 8)
                .map(|entry| entry.element_id.as_str()),
            Some("strong")
        );
        assert_eq!(sync.leaves_in_range(&file, 1, 8, 1, 8), vec!["strong"]);
    }

    #[test]
    fn zero_width_spans_still_match_their_anchor_position() {
        let file = PathBuf::from("/tmp/main.md");
        let mut sync = SyncIndex::new();
        sync.record("anchor", file.clone(), pos(2, 4), pos(2, 4), None);

        assert_eq!(
            sync.lookup_leaf_by_source_position(&file, 2, 4)
                .map(|entry| entry.element_id.as_str()),
            Some("anchor")
        );
        assert_eq!(sync.leaves_in_range(&file, 2, 4, 2, 4), vec!["anchor"]);
        assert!(sync.leaves_in_range(&file, 2, 5, 2, 5).is_empty());
    }

    #[test]
    fn containing_container_lookup_never_snaps_to_a_nearby_container() {
        let file = PathBuf::from("/tmp/main.md");
        let mut sync = SyncIndex::new();
        sync.record_with_kind(
            "code",
            file.clone(),
            pos(5, 1),
            pos(9, 1),
            None,
            SyncKind::Container,
        );

        assert_eq!(
            sync.lookup_containing_container_by_source_position(&file, 7, 3)
                .map(|entry| entry.element_id.as_str()),
            Some("code")
        );
        assert!(sync
            .lookup_containing_container_by_source_position(&file, 10, 1)
            .is_none());
    }

    fn row(start_line: u32, end_line: u32, start_col: u32) -> MathRow {
        MathRow {
            start_line,
            end_line,
            start_col,
        }
    }

    #[test]
    fn math_rows_in_range_returns_overlapping_row_indices() {
        let file = PathBuf::from("/tmp/main.tex");
        let mut sync = SyncIndex::new();
        // A 3-row align with rows on source lines 4, 5, 6.
        sync.record_math_rows(
            "dm-1",
            file.clone(),
            vec![row(4, 4, 3), row(5, 5, 3), row(6, 6, 3)],
        );

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

    #[test]
    fn math_row_pos_resolves_clicked_row_and_guards_skew() {
        let file = PathBuf::from("/tmp/main.tex");
        let mut sync = SyncIndex::new();
        sync.record_math_rows(
            "dm-1",
            file.clone(),
            vec![row(4, 4, 0), row(5, 6, 3), row(7, 7, 5)],
        );

        // The clicked row's own position, including its start column.
        assert_eq!(sync.math_row_pos("dm-1", &file, 1, 3), Some((5, 3)));
        // Row 0 starts on the \begin line: col is the 0 = unknown sentinel.
        assert_eq!(sync.math_row_pos("dm-1", &file, 0, 3), Some((4, 0)));
        // Rendered/source row-count skew → refuse, caller falls back.
        assert_eq!(sync.math_row_pos("dm-1", &file, 1, 2), None);
        // Row index out of bounds → refuse.
        assert_eq!(sync.math_row_pos("dm-1", &file, 3, 3), None);
        // Unknown block or wrong file → refuse.
        assert_eq!(sync.math_row_pos("dm-9", &file, 0, 3), None);
        assert_eq!(
            sync.math_row_pos("dm-1", Path::new("/tmp/other.tex"), 0, 3),
            None
        );
    }
}
