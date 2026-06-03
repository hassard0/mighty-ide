//! Problems panel (shim-side): aggregated diagnostics across the open tabs /
//! workspace, grouped by file, click-to-jump.
//!
//! Reuses [`crate::diagnostics`] for Mighty files and accepts already-parsed
//! diagnostic lists from the generic LSP bridge for other languages. The set is
//! sorted (file, then line, then col) and the panel renders file-group headers +
//! indented rows. It is a BOTTOM panel (same band the Run panel uses) so it
//! reads like a problems dock; clicking the status-bar problems chip opens it
//! (wired in main.mty).
//!
//! Placement note: the Run panel and this panel share the bottom band — only one
//! is shown at a time (opening Problems closes Run and vice-versa in the IDE),
//! so they never overlap.

use std::path::{Path, PathBuf};

use crate::diagnostics::{self, Severity};
use crate::layout;
use crate::theme;

fn fit_ui_text(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    let max_px = max_px.max(0.0);
    if max_px <= 1.0 {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    let ellipsis_w = text.measure_ui_sized(ellipsis, size).0;
    if ellipsis_w >= max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut candidate: String = chars.iter().take(mid).collect();
        candidate.push_str(ellipsis);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return ellipsis.to_string();
    }
    let mut out: String = chars.iter().take(lo).collect();
    out.push_str(ellipsis);
    out
}

fn problem_ui_text_width(text: &mut crate::text::Text, s: &str, size: f32) -> f32 {
    text.measure_ui_sized(s, size).0
}

pub(crate) fn compact_problem_rows(panel_w: f32) -> bool {
    panel_w < 360.0
}

pub(crate) fn problem_location_label(line: i32, col: i32, compact: bool) -> String {
    if compact {
        format!("{}:{}", line + 1, col + 1)
    } else {
        format!("Ln {}, Col {}", line + 1, col + 1)
    }
}

pub(crate) fn problem_message_budget(
    msg_x: f32,
    location_x: f32,
    code_x: Option<f32>,
) -> f32 {
    let right_x = code_x.unwrap_or(location_x);
    right_x - 10.0 - msg_x
}

/// One aggregated problem: an owning file plus the underlying diagnostic fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Absolute path of the file the diagnostic belongs to.
    pub path: PathBuf,
    /// Basename, cached for icon lookup and compact display.
    pub file: String,
    /// Header label. Usually the basename; expanded to the path when duplicate
    /// basenames would otherwise collapse distinct files into one group.
    pub label: String,
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
    /// testable; callers can feed either Mighty or generic-LSP diagnostics.
    pub fn aggregate(&mut self, lists: Vec<(PathBuf, Vec<diagnostics::Diag>)>) -> usize {
        let mut items: Vec<Problem> = Vec::new();
        for (path, diags) in lists {
            let file = basename(&path);
            for d in diags {
                items.push(Problem {
                    path: path.clone(),
                    file: file.clone(),
                    label: file.clone(),
                    line: d.line,
                    col: d.col_start,
                    severity: d.severity,
                    code: d.code,
                    message: d.message,
                });
            }
        }
        disambiguate_duplicate_basenames(&mut items);
        // Sort: by file, then path, then line/column (stable grouping for the panel).
        items.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.path.cmp(&b.path))
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

    /// The number of distinct files with problems.
    pub fn file_count(&self) -> usize {
        let mut files: Vec<&Path> = self.items.iter().map(|p| p.path.as_path()).collect();
        files.dedup();
        files.len()
    }

    /// Build the flattened visual row list: a `FileHeader` per group followed by
    /// its problem rows. Used by both the click hit-test and the draw so they
    /// agree on geometry.
    fn visual_rows(&self) -> Vec<VisRow> {
        let mut rows = Vec::new();
        let mut last_path: Option<&Path> = None;
        for (i, p) in self.items.iter().enumerate() {
            if last_path != Some(p.path.as_path()) {
                rows.push(VisRow::FileHeader(i));
                last_path = Some(p.path.as_path());
            }
            rows.push(VisRow::Problem(i));
        }
        rows
    }

    /// The problem-row band's top y (just under the header).
    fn body_top(h: f32) -> f32 {
        Self::panel_top(h) + layout::term_header_h()
    }

    /// The panel's top y. Problems shares the same lower-dock geometry as
    /// Terminal/Run/Web so editor row reservation, resize handle, and click
    /// targets agree.
    fn panel_top(h: f32) -> f32 {
        layout::term_panel_top(h.max(1.0) as u32)
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

    /// Hit-test the header close affordance.
    pub fn close_at(&self, click_x: f32, click_y: f32, w: f32, h: f32, left: f32) -> bool {
        if !self.open || click_x < left || click_x > w {
            return false;
        }
        let (x, y, cw, ch) = layout::dock_close_rect(w.max(1.0) as u32, h.max(1.0) as u32);
        click_x >= x && click_x <= x + cw && click_y >= y && click_y <= y + ch
    }

    /// Draw the Problems panel as a bottom band: a header with error/warning
    /// totals, then file groups, then indented `severity message code Ln:Col`
    /// rows. No-op when closed.
    pub fn draw(&self, ctx: &mut crate::MuiContext, left: f32) {
        if !self.open {
            return;
        }
        let w = layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width) as f32;
        let h = ctx.gpu.height as f32;
        let clip = ctx.clip;
        let chrome = theme::CHROME_FONT_SIZE;
        let top = Self::panel_top(h);
        let panel_h = layout::term_panel_height(ctx.gpu.height);
        let panel_w = w - left;
        let compact_rows = compact_problem_rows(panel_w);

        // Panel surface (elevated) + a top divider with a faint glow line.
        ctx.dl_rect(left, top, w - left, panel_h, theme::BG_1());
        ctx.dl_rect(left, top, w - left, 1.0, theme::BORDER());
        ctx.dl_shadow(left, top, w - left, 2.0, 0.0, theme::ACCENT_GLOW(), 6.0);

        // Header band.
        let head_h = layout::term_header_h();
        ctx.dl_grad_v(left, top, w - left, head_h, 0.0, theme::BG_2(), theme::BG_1());
        ctx.dl_rect(left, top + head_h - 1.0, w - left, 1.0, theme::BORDER_SOFT());

        use crate::icons;
        let hy = top + (head_h - (chrome - 1.0)) * 0.5 - 1.0;
        let iy = top + (head_h - 13.0) * 0.5;
        let mut x = left + 14.0;
        let heading = "PROBLEMS";
        ctx.text.queue_ui_sized(x, hy, heading, theme::DIM(), chrome - 1.0, clip);
        x += problem_ui_text_width(&mut ctx.text, heading, chrome - 1.0) + 18.0;

        // Error count chip.
        ctx.dl_icon(x, iy, 13.0, 13.0, icons::ERROR_CIRCLE, theme::ERROR(), 1.5, false);
        x += 17.0;
        let ec = self.errors.to_string();
        ctx.text.queue_ui_sized(x, hy, &ec, if self.errors > 0 { theme::ERROR() } else { theme::TEXT_3() }, chrome - 1.0, clip);
        x += problem_ui_text_width(&mut ctx.text, &ec, chrome - 1.0) + 12.0;
        // Warning count chip.
        ctx.dl_icon(x, iy, 13.0, 13.0, icons::WARN_TRI, theme::WARNING(), 1.5, false);
        x += 17.0;
        let wc = self.warnings.to_string();
        ctx.text.queue_ui_sized(x, hy, &wc, if self.warnings > 0 { theme::WARNING() } else { theme::TEXT_3() }, chrome - 1.0, clip);

        if self.items.is_empty() {
            ctx.dl_icon(left + 14.0, Self::body_top(h) + 2.0, 14.0, 14.0, icons::CHECK, theme::GREEN(), 1.7, false);
            let msg_x = left + 36.0;
            let msg = fit_ui_text(
                &mut ctx.text,
                "No problems detected in the workspace.",
                w - msg_x - 14.0,
                chrome,
            );
            if !msg.is_empty() {
                ctx.text.queue_ui_sized(msg_x, Self::body_top(h) + 2.0, &msg, theme::TEXT_3(), chrome, clip);
            }
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
                    let file_x = left + 46.0;
                    let file = fit_ui_text(&mut ctx.text, &p.label, w - file_x - 14.0, chrome);
                    if !file.is_empty() {
                        ctx.text.queue_ui_sized(file_x, y + (row_h - chrome) * 0.5 - 1.0, &file, theme::TEXT_1(), chrome, clip);
                    }
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
                    let lc = problem_location_label(p.line, p.col, compact_rows);
                    let lc_w = problem_ui_text_width(&mut ctx.text, &lc, chrome - 1.0);
                    let code_w = if compact_rows {
                        0.0
                    } else {
                        problem_ui_text_width(&mut ctx.text, &p.code, chrome - 1.0)
                    };
                    let rx_lc = w - 14.0 - lc_w;
                    let rx_code = if compact_rows { rx_lc } else { rx_lc - 12.0 - code_w };
                    ctx.text.queue_ui_sized(rx_lc, y + (row_h - (chrome - 1.0)) * 0.5 - 1.0, &lc, theme::TEXT_4(), chrome - 1.0, clip);
                    if !compact_rows && !p.code.is_empty() {
                        ctx.text.queue_ui_sized(rx_code, y + (row_h - (chrome - 1.0)) * 0.5 - 1.0, &p.code, theme::TEXT_3(), chrome - 1.0, clip);
                    }
                    // Message, measured and clipped before the right cluster.
                    let code_x = if compact_rows || p.code.is_empty() { None } else { Some(rx_code) };
                    let msg = fit_ui_text(&mut ctx.text, &p.message, problem_message_budget(msg_x, rx_lc, code_x), chrome);
                    if !msg.is_empty() {
                        ctx.text.queue_ui_sized(msg_x, y + (row_h - chrome) * 0.5 - 1.0, &msg, theme::TEXT(), chrome, clip);
                    }
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

fn disambiguate_duplicate_basenames(items: &mut [Problem]) {
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for item in items.iter() {
        if let Some((_, paths)) = groups.iter_mut().find(|(file, _)| file == &item.file) {
            if !paths.contains(&item.path) {
                paths.push(item.path.clone());
            }
        } else {
            groups.push((item.file.clone(), vec![item.path.clone()]));
        }
    }

    for item in items.iter_mut() {
        let duplicate = groups
            .iter()
            .find(|(file, _)| file == &item.file)
            .map(|(_, paths)| paths.len() > 1)
            .unwrap_or(false);
        item.label = if duplicate {
            item.path.to_string_lossy().into_owned()
        } else {
            item.file.clone()
        };
    }
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
    fn aggregate_keeps_duplicate_basenames_as_separate_file_groups() {
        let mut ps = ProblemSet::new();
        ps.aggregate(vec![
            (
                PathBuf::from("/ws/app/src/main.rs"),
                vec![diag(0, 0, Severity::Error, "E1", "app error")],
            ),
            (
                PathBuf::from("/ws/tool/src/main.rs"),
                vec![diag(1, 0, Severity::Warning, "W1", "tool warning")],
            ),
        ]);

        assert_eq!(ps.file_count(), 2);
        assert_eq!(ps.count(), 2);
        assert_eq!(ps.get(0).unwrap().file, "main.rs");
        assert!(ps.get(0).unwrap().label.contains("app"));
        assert!(ps.get(1).unwrap().label.contains("tool"));

        let headers = ps
            .visual_rows()
            .into_iter()
            .filter(|row| matches!(row, VisRow::FileHeader(_)))
            .count();
        assert_eq!(headers, 2);
    }

    #[test]
    fn compact_problem_rows_use_short_location_and_hide_code_budget() {
        assert!(compact_problem_rows(282.0));
        assert!(!compact_problem_rows(420.0));
        assert_eq!(problem_location_label(6, 13, true), "7:14");
        assert_eq!(problem_location_label(6, 13, false), "Ln 7, Col 14");

        let compact_budget = problem_message_budget(332.0, 515.0, None);
        let wide_budget = problem_message_budget(332.0, 515.0, Some(450.0));
        assert!(
            compact_budget > wide_budget,
            "compact rows should give the message the code column's space"
        );
    }

    #[test]
    fn problem_ui_text_width_uses_measured_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };
        let short = problem_ui_text_width(&mut ctx.text, "1", theme::CHROME_FONT_SIZE - 1.0);
        let long = problem_ui_text_width(&mut ctx.text, "Ln 128, Col 64", theme::CHROME_FONT_SIZE - 1.0);

        assert!(short > 0.0);
        assert!(long > short);
    }

    #[test]
    fn measured_problem_right_cluster_tightens_message_budget() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };
        let chrome = theme::CHROME_FONT_SIZE;
        let panel_right = 620.0;
        let msg_x = 120.0;
        let lc = problem_location_label(128, 64, false);
        let lc_w = problem_ui_text_width(&mut ctx.text, &lc, chrome - 1.0);
        let code_w = problem_ui_text_width(&mut ctx.text, "MT4001", chrome - 1.0);
        let rx_lc = panel_right - 14.0 - lc_w;
        let rx_code = rx_lc - 12.0 - code_w;
        let wide_budget = problem_message_budget(msg_x, rx_lc, Some(rx_code));
        let compact_budget = problem_message_budget(msg_x, rx_lc, None);

        assert!(rx_code < rx_lc);
        assert!(wide_budget < compact_budget);
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

    #[test]
    fn close_hit_test_targets_header_button_only() {
        let mut ps = ProblemSet::new();
        ps.set_open(true);
        let (x, y, w, h) = layout::dock_close_rect(1000, 800);
        assert!(ps.close_at(x + w * 0.5, y + h * 0.5, 1000.0, 800.0, 52.0));
        assert!(!ps.close_at(x - 8.0, y + h * 0.5, 1000.0, 800.0, 52.0));
        assert!(!ps.close_at(x + w * 0.5, y + h + 8.0, 1000.0, 800.0, 52.0));
    }
}
