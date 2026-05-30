//! Problems panel (shim-side): aggregated `mty check` diagnostics across the
//! open tabs / workspace, grouped by file, click-to-jump.
//!
//! Reuses [`crate::diagnostics`] (the same `mty check` runner + parser the
//! editor squiggles use). [`ProblemSet::refresh`] runs `check` on the active
//! file and any other open `.mty` tabs, collecting one [`Problem`] per
//! diagnostic with its owning file. The set is then sorted (file, then line,
//! then col) and the panel renders file-group headers + indented rows. It is a
//! BOTTOM panel (same band the Run panel uses) so it reads like a problems dock;
//! clicking the status-bar problems chip opens it (wired in main.mty).
//!
//! Placement note: the Run panel and this panel share the bottom band — only one
//! is shown at a time (opening Problems closes Run and vice-versa in the IDE),
//! so they never overlap.

use std::path::{Path, PathBuf};

use crate::diagnostics::{self, Severity};
use crate::layout;
use crate::theme;

/// One aggregated problem: an owning file plus the underlying diagnostic fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Absolute path of the file the diagnostic belongs to.
    pub path: PathBuf,
    /// Basename, cached for display + grouping.
    pub file: String,
    /// 0-based line.
    pub line: i32,
    /// 0-based start column.
    pub col: i32,
    pub severity: Severity,
    pub code: String,
    pub message: String,
}

/// The Problems panel state: the aggregated list + open flag + scroll + counts.
#[derive(Debug, Default)]
pub struct ProblemSet {
    items: Vec<Problem>,
    open: bool,
    scroll: i32,
    errors: i32,
    warnings: i32,
}

impl ProblemSet {
    pub fn new() -> Self {
        ProblemSet::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        self.open
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    pub fn error_count(&self) -> i32 {
        self.errors
    }

    pub fn warn_count(&self) -> i32 {
        self.warnings
    }

    pub fn get(&self, i: usize) -> Option<&Problem> {
        self.items.get(i)
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll + delta).max(0);
        let max = self.items.len() as i32;
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Build the aggregated set from already-parsed per-file diagnostic lists.
    /// `lists` is `(path, diags)` per file. Pure (no subprocess) so it is unit
    /// testable; [`refresh`](Self::refresh) is the side-effecting wrapper.
    pub fn aggregate(&mut self, lists: Vec<(PathBuf, Vec<diagnostics::Diag>)>) -> usize {
        let mut items: Vec<Problem> = Vec::new();
        for (path, diags) in lists {
            let file = basename(&path);
            for d in diags {
                items.push(Problem {
                    path: path.clone(),
                    file: file.clone(),
                    line: d.line,
                    col: d.col_start,
                    severity: d.severity,
                    code: d.code,
                    message: d.message,
                });
            }
        }
        // Sort: by file, then line, then column (stable grouping for the panel).
        items.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line.cmp(&b.line))
                .then(a.col.cmp(&b.col))
        });
        self.errors = items.iter().filter(|p| p.severity == Severity::Error).count() as i32;
        self.warnings = items.iter().filter(|p| p.severity == Severity::Warning).count() as i32;
        self.items = items;
        if self.scroll > self.items.len() as i32 {
            self.scroll = 0;
        }
        self.items.len()
    }

    /// Run `mty check` on every distinct path in `paths` and aggregate. Skips
    /// duplicates + non-`.mty` files. The active file should be first in `paths`.
    pub fn refresh(&mut self, paths: &[PathBuf]) -> usize {
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut lists: Vec<(PathBuf, Vec<diagnostics::Diag>)> = Vec::new();
        for p in paths {
            if seen.contains(p) {
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("mty") {
                continue;
            }
            if !p.exists() {
                continue;
            }
            seen.push(p.clone());
            let diags = diagnostics::run_check(p);
            if !diags.is_empty() {
                lists.push((p.clone(), diags));
            }
        }
        self.aggregate(lists)
    }

    /// The number of distinct files with problems.
    pub fn file_count(&self) -> usize {
        let mut files: Vec<&str> = self.items.iter().map(|p| p.file.as_str()).collect();
        files.dedup();
        files.len()
    }

    /// Build the flattened visual row list: a `FileHeader` per group followed by
    /// its problem rows. Used by both the click hit-test and the draw so they
    /// agree on geometry.
    fn visual_rows(&self) -> Vec<VisRow> {
        let mut rows = Vec::new();
        let mut last_file: Option<&str> = None;
        for (i, p) in self.items.iter().enumerate() {
            if last_file != Some(p.file.as_str()) {
                rows.push(VisRow::FileHeader(i));
                last_file = Some(p.file.as_str());
            }
            rows.push(VisRow::Problem(i));
        }
        rows
    }

    /// The problem-row band's top y (just under the header).
    fn body_top(h: f32) -> f32 {
        Self::panel_top(h) + 32.0
    }

    /// The panel's top y (a bottom band ~38% of the window, min 180px).
    fn panel_top(h: f32) -> f32 {
        let panel_h = (h * 0.34).max(180.0).min(h - 120.0);
        (h - 30.0 - panel_h).max(0.0)
    }

    /// Map a click y (window coords) + the editor left edge to a problem index,
    /// or `-1` for a header row / outside. `left` is the editor body's left edge.
    pub fn row_at(&self, click_x: f32, click_y: f32, w: f32, h: f32, left: f32) -> i32 {
        if !self.open {
            return -1;
        }
        if click_x < left || click_x > w {
            return -1;
        }
        let top = Self::body_top(h);
        if click_y < top {
            return -1;
        }
        let row_h = layout::LINE_H();
        let idx = ((click_y - top) / row_h).floor() as i32 + self.scroll;
        let rows = self.visual_rows();
        if idx < 0 || idx as usize >= rows.len() {
            return -1;
        }
        match rows[idx as usize] {
            VisRow::Problem(pi) => pi as i32,
            VisRow::FileHeader(_) => -1,
        }
    }

    /// Draw the Problems panel as a bottom band: a header with error/warning
    /// totals, then file groups, then indented `severity message code Ln:Col`
    /// rows. No-op when closed.
    pub fn draw(&self, ctx: &mut crate::MuiContext, left: f32) {
        if !self.open {
            return;
        }
        let w = ctx.gpu.width as f32;
        let h = ctx.gpu.height as f32;
        let clip = ctx.clip;
        let chrome = theme::CHROME_FONT_SIZE;
        let adv = chrome * 0.55;
        let top = Self::panel_top(h);
        let panel_h = h - 30.0 - top;

        // Panel surface (elevated) + a top divider with a faint glow line.
        ctx.dl_rect(left, top, w - left, panel_h, theme::BG_1());
        ctx.dl_rect(left, top, w - left, 1.0, theme::BORDER());
        ctx.dl_shadow(left, top, w - left, 2.0, 0.0, theme::ACCENT_GLOW(), 6.0);

        // Header band.
        let head_h = 30.0;
        ctx.dl_grad_v(left, top, w - left, head_h, 0.0, theme::BG_2(), theme::BG_1());
        ctx.dl_rect(left, top + head_h - 1.0, w - left, 1.0, theme::BORDER_SOFT());

        use crate::icons;
        let hy = top + (head_h - (chrome - 1.0)) * 0.5 - 1.0;
        let iy = top + (head_h - 13.0) * 0.5;
        let mut x = left + 14.0;
        ctx.text.queue_ui_sized(x, hy, "PROBLEMS", theme::DIM(), chrome - 1.0, clip);
        x += "PROBLEMS".chars().count() as f32 * adv + 18.0;

        // Error count chip.
        ctx.dl_icon(x, iy, 13.0, 13.0, icons::ERROR_CIRCLE, theme::ERROR(), 1.5, false);
        x += 17.0;
        let ec = self.errors.to_string();
        ctx.text.queue_ui_sized(x, hy, &ec, if self.errors > 0 { theme::ERROR() } else { theme::TEXT_3() }, chrome - 1.0, clip);
        x += ec.chars().count() as f32 * adv + 12.0;
        // Warning count chip.
        ctx.dl_icon(x, iy, 13.0, 13.0, icons::WARN_TRI, theme::WARNING(), 1.5, false);
        x += 17.0;
        let wc = self.warnings.to_string();
        ctx.text.queue_ui_sized(x, hy, &wc, if self.warnings > 0 { theme::WARNING() } else { theme::TEXT_3() }, chrome - 1.0, clip);

        if self.items.is_empty() {
            ctx.dl_icon(left + 14.0, Self::body_top(h) + 2.0, 14.0, 14.0, icons::CHECK, theme::GREEN(), 1.7, false);
            ctx.text.queue_ui_sized(left + 36.0, Self::body_top(h) + 2.0, "No problems detected in the workspace.", theme::TEXT_3(), chrome, clip);
            return;
        }

        let rows = self.visual_rows();
        let row_h = layout::LINE_H();
        let body_top = Self::body_top(h);
        let mut vi = 0usize;
        for (ri, row) in rows.iter().enumerate() {
            if (ri as i32) < self.scroll {
                continue;
            }
            let y = body_top + (vi as f32) * row_h;
            if y + row_h > h - 30.0 {
                break;
            }
            vi += 1;
            match *row {
                VisRow::FileHeader(pi) => {
                    let p = &self.items[pi];
                    let (icon, icol) = crate::abi::file_icon_for(&p.file, false);
                    ctx.dl_icon(left + 12.0, y + (row_h - 13.0) * 0.5, 12.0, 12.0, icons::CHEVRON_DOWN, theme::TEXT_3(), 2.0, false);
                    ctx.dl_icon(left + 28.0, y + (row_h - 14.0) * 0.5, 14.0, 14.0, icon, icol, 1.4, false);
                    ctx.text.queue_ui_sized(left + 46.0, y + (row_h - chrome) * 0.5 - 1.0, &p.file, theme::TEXT_1(), chrome, clip);
                }
                VisRow::Problem(pi) => {
                    let p = &self.items[pi];
                    let (sicon, scol) = match p.severity {
                        Severity::Error => (icons::ERROR_CIRCLE, theme::ERROR()),
                        Severity::Warning => (icons::WARN_TRI, theme::WARNING()),
                    };
                    let sx = left + 34.0;
                    ctx.dl_icon(sx, y + (row_h - 13.0) * 0.5, 13.0, 13.0, sicon, scol, 1.5, false);
                    let msg_x = sx + 20.0;
                    // Right cluster: code + Ln:Col, laid out from the right.
                    let lc = format!("Ln {}, Col {}", p.line + 1, p.col + 1);
                    let lc_w = lc.chars().count() as f32 * (chrome - 1.0) * 0.55;
                    let code_w = p.code.chars().count() as f32 * (chrome - 1.0) * 0.55;
                    let rx_lc = w - 14.0 - lc_w;
                    let rx_code = rx_lc - 12.0 - code_w;
                    ctx.text.queue_ui_sized(rx_lc, y + (row_h - (chrome - 1.0)) * 0.5 - 1.0, &lc, theme::TEXT_4(), chrome - 1.0, clip);
                    if !p.code.is_empty() {
                        ctx.text.queue_ui_sized(rx_code, y + (row_h - (chrome - 1.0)) * 0.5 - 1.0, &p.code, theme::TEXT_3(), chrome - 1.0, clip);
                    }
                    // Message, clipped before the right cluster.
                    let avail = ((rx_code - 8.0 - msg_x) / adv).floor() as usize;
                    let mut msg = p.message.clone();
                    if msg.chars().count() > avail && avail > 1 {
                        msg = msg.chars().take(avail - 1).collect::<String>() + "\u{2026}";
                    }
                    ctx.text.queue_ui_sized(msg_x, y + (row_h - chrome) * 0.5 - 1.0, &msg, theme::TEXT(), chrome, clip);
                }
            }
        }
    }
}

/// A visual row: a file group header (carrying the first problem index of the
/// group) or a problem row (carrying its problem index).
enum VisRow {
    FileHeader(usize),
    Problem(usize),
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diag;

    fn diag(line: i32, col: i32, sev: Severity, code: &str, msg: &str) -> Diag {
        Diag {
            line,
            col_start: col,
            col_end: col + 1,
            severity: sev,
            code: code.into(),
            message: msg.into(),
        }
    }

    #[test]
    fn aggregate_groups_and_sorts() {
        let mut ps = ProblemSet::new();
        let n = ps.aggregate(vec![
            (
                PathBuf::from("/ws/b.mty"),
                vec![diag(5, 2, Severity::Warning, "MT3001", "unused x")],
            ),
            (
                PathBuf::from("/ws/a.mty"),
                vec![
                    diag(10, 0, Severity::Error, "MT2002", "second"),
                    diag(2, 4, Severity::Error, "MT2001", "first"),
                ],
            ),
        ]);
        assert_eq!(n, 3);
        // Sorted by file (a before b), then line.
        assert_eq!(ps.get(0).unwrap().file, "a.mty");
        assert_eq!(ps.get(0).unwrap().line, 2);
        assert_eq!(ps.get(0).unwrap().code, "MT2001");
        assert_eq!(ps.get(1).unwrap().file, "a.mty");
        assert_eq!(ps.get(1).unwrap().line, 10);
        assert_eq!(ps.get(2).unwrap().file, "b.mty");
    }

    #[test]
    fn aggregate_counts_severities() {
        let mut ps = ProblemSet::new();
        ps.aggregate(vec![(
            PathBuf::from("/ws/a.mty"),
            vec![
                diag(0, 0, Severity::Error, "MT1", "e1"),
                diag(1, 0, Severity::Error, "MT2", "e2"),
                diag(2, 0, Severity::Warning, "MT3", "w1"),
            ],
        )]);
        assert_eq!(ps.error_count(), 2);
        assert_eq!(ps.warn_count(), 1);
        assert_eq!(ps.file_count(), 1);
    }

    #[test]
    fn aggregate_multi_file_count() {
        let mut ps = ProblemSet::new();
        ps.aggregate(vec![
            (PathBuf::from("/ws/a.mty"), vec![diag(0, 0, Severity::Error, "MT1", "e")]),
            (PathBuf::from("/ws/b.mty"), vec![diag(0, 0, Severity::Error, "MT2", "e")]),
        ]);
        assert_eq!(ps.file_count(), 2);
        assert_eq!(ps.count(), 2);
    }

    #[test]
    fn empty_aggregate_is_clean() {
        let mut ps = ProblemSet::new();
        assert_eq!(ps.aggregate(vec![]), 0);
        assert_eq!(ps.error_count(), 0);
        assert_eq!(ps.warn_count(), 0);
        assert_eq!(ps.file_count(), 0);
    }

    #[test]
    fn open_toggle() {
        let mut ps = ProblemSet::new();
        assert!(!ps.is_open());
        assert!(ps.toggle());
        assert!(ps.is_open());
        ps.set_open(false);
        assert!(!ps.is_open());
    }

    #[test]
    fn row_at_when_closed_is_negative() {
        let ps = ProblemSet::new();
        assert_eq!(ps.row_at(100.0, 500.0, 1000.0, 800.0, 52.0), -1);
    }
}
