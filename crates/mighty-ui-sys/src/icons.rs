//! Vector icon registry — real SVG path data extracted from the approved
//! `design/option-b.html` "Vivid Modern" mockup.
//!
//! Each constant is the `d=` path data of an inline `<svg>` icon in a 24x24
//! viewBox. The Vello backend ([`crate::vello_ui::UiCmd::Icon`]) parses these
//! with `kurbo::BezPath::from_svg`, scales them into a target box, and
//! fills/strokes them. This replaces the old unicode-glyph "icons", which were
//! the single biggest source of the "ugly / not aligned" feedback.
//!
//! Stroke icons are drawn at ~1.5px (scaled); a few (play triangle, dots,
//! badges) are filled. All viewBoxes are 24.0 unless noted.

#![allow(dead_code)]

/// The conventional source viewBox edge length for these icons.
pub const VB: f32 = 24.0;

// ---- Activity rail ----
/// Explorer (files / folder-with-tab).
pub const EXPLORER: &str = "M4 5.5A1.5 1.5 0 0 1 5.5 4h3.7a1.5 1.5 0 0 1 1.06.44L11.5 5.5h7A1.5 1.5 0 0 1 20 7v11.5a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 18.5z";
/// Search magnifier (circle + handle, combined path).
pub const SEARCH: &str = "M11 4.5a6.5 6.5 0 1 0 4.6 11.1L19.8 19.8 M11 4.5a6.5 6.5 0 0 1 0 13 6.5 6.5 0 0 1 0-13z";
/// Source control (git graph: three nodes + connectors). Drawn as separate
/// sub-paths so circles + lines render together.
pub const GIT: &str = "M6.5 3.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z M6.5 15.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z M17.5 6a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z M6.5 8.5v7 M17.5 11c0 4-3.5 3.5-7 4.5";
/// Run / play triangle (filled).
pub const RUN: &str = "M7 5.5 18 12 7 18.5z";
/// Agents (robot head).
pub const AGENTS: &str = "M5 8h14a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z M12 5v3 M9.5 16h5";

/// Debug (a ladybug / "play-bug" — Run and Debug rail icon).
pub const DEBUG: &str = "M9 8a3 3 0 0 1 6 0 M8.5 8h7a1 1 0 0 1 1 1v3a4.5 4.5 0 0 1-9 0V9a1 1 0 0 1 1-1z M5 10H2.5 M5 14H2.5 M5 18l1.5-2 M19 10h2.5 M19 14h2.5 M19 18l-1.5-2 M12 13v6";
/// Debug controls (filled / stroked) used by the debug toolbar.
pub const DBG_CONTINUE: &str = "M7 5.5 18 12 7 18.5z";
pub const DBG_STOP: &str = "M6.5 6.5h11v11h-11z";
pub const DBG_STEP_OVER: &str = "M5 10a7 7 0 0 1 13 1.5 M18 6.5V12h-5.5 M12 16.5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3z";
pub const DBG_STEP_INTO: &str = "M12 4v9 M8.5 9.5 12 13l3.5-3.5 M12 17.5a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z";
pub const DBG_STEP_OUT: &str = "M12 13V4 M8.5 7.5 12 4l3.5 3.5 M12 17.5a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z";
/// Solid breakpoint dot path (filled circle in the gutter).
pub const BREAKPOINT: &str = "M12 6a6 6 0 1 0 0 12 6 6 0 0 0 0-12z";
/// Current-instruction arrow (filled), drawn in the gutter at the stopped line.
pub const DBG_ARROW: &str = "M5 8.5h7V5l7 7-7 7v-3.5H5z";
/// Agent eyes/antenna dot (filled), used together with AGENTS.
pub const AGENTS_DOT: &str = "M12 2.6a1.4 1.4 0 1 0 0 2.8 1.4 1.4 0 0 0 0-2.8z M9.5 11.9a1.1 1.1 0 1 0 0 2.2 1.1 1.1 0 0 0 0-2.2z M14.5 11.9a1.1 1.1 0 1 0 0 2.2 1.1 1.1 0 0 0 0-2.2z";
/// Accounts (user).
pub const USER: &str = "M12 5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z M5 19.5c0-3.3 3.1-5.5 7-5.5s7 2.2 7 5.5";
/// Settings gear.
pub const SETTINGS: &str = "M12 9.2a2.8 2.8 0 1 0 0 5.6 2.8 2.8 0 0 0 0-5.6z M12 3.5v2M12 18.5v2M20.5 12h-2M5.5 12h-2M18 6l-1.4 1.4M7.4 16.6 6 18M18 18l-1.4-1.4M7.4 7.4 6 6";

// ---- File-tree / tabs ----
/// Folder (closed).
pub const FOLDER: &str = "M4 7a1 1 0 0 1 1-1h3l1.5 1.5H19a1 1 0 0 1 1 1V18a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1z";
/// `.mty` file (rounded doc + a downward chevron mark).
pub const FILE_MTY: &str = "M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v13A2.5 2.5 0 0 1 17.5 21h-11A2.5 2.5 0 0 1 4 18.5z M8.5 15.5V9l3 3 3-3v6.5";
/// `.toml` file (doc + three lines, last short).
pub const FILE_TOML: &str = "M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v13A2.5 2.5 0 0 1 17.5 21h-11A2.5 2.5 0 0 1 4 18.5z M7.5 8h9M7.5 11.5h9M7.5 15h5";
/// `.md` file (doc + three lines).
pub const FILE_MD: &str = "M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v13A2.5 2.5 0 0 1 17.5 21h-11A2.5 2.5 0 0 1 4 18.5z M8 8h8M8 12h8M8 16h4";
/// `.txt` / generic file (doc + three lines).
pub const FILE_TXT: &str = "M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v13A2.5 2.5 0 0 1 17.5 21h-11A2.5 2.5 0 0 1 4 18.5z M8 9h8M8 12.5h8M8 16h6";

// ---- Misc chrome ----
/// Chevron-right (collapsed disclosure / breadcrumb separator).
pub const CHEVRON: &str = "M9 6l6 6-6 6";
/// Tab close ×.
pub const CLOSE: &str = "M6 6l12 12M18 6 6 18";
/// Window minimize (a single horizontal bar near the middle).
pub const WIN_MIN: &str = "M6 12h12";
/// Window maximize (a square outline).
pub const WIN_MAX: &str = "M6 6h12v12H6z";
/// Window restore (two offset squares — shown when maximized).
pub const WIN_RESTORE: &str = "M8 8V6h10v10h-2 M6 9h10v9H6z";
/// More (horizontal three dots) — filled.
pub const DOTS: &str = "M6 10.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z M12 10.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z M18 10.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z";
/// New file (doc with folded corner).
pub const NEW_FILE: &str = "M13 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9z M13 3v6h6";
/// New folder (folder with +).
pub const NEW_FOLDER: &str = "M4 7a1 1 0 0 1 1-1h3l1.5 1.5H19a1 1 0 0 1 1 1V18a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1z M12 11v4M10 13h4";
/// Collapse-all.
pub const COLLAPSE: &str = "M5 8h6V2M19 16h-6v6M5 8l5-5M19 16l-5 5";

// ---- Breadcrumb symbol (function) ----
/// `fn` symbol marker for the breadcrumb (function-ish glyph).
pub const FN_SYMBOL: &str = "M5 12h6V6m8 6h-6v6";

// ---- Outline (document symbols) rail icon ----
/// Outline / symbols list (a small tree: a stem with branch dots + lines).
pub const OUTLINE: &str = "M5 6h3M5 12h3M5 18h3 M10.5 6h8.5M10.5 12h8.5M10.5 18h5 M6.5 6.8v10.4";

// ---- Mighty Agents (topology) rail icon ----
/// Agent network / nodes graph (the Mighty Agents rail icon, slot 8): three
/// nodes connected by edges — a message-passing topology glyph. Distinct from
/// the AI-copilot robot head (`AGENTS`, slot 4) so the two never read alike.
pub const AGENTS_NET: &str = "M6 5.5a2.2 2.2 0 1 0 0 4.4 2.2 2.2 0 0 0 0-4.4z M18 5.5a2.2 2.2 0 1 0 0 4.4 2.2 2.2 0 0 0 0-4.4z M12 14a2.2 2.2 0 1 0 0 4.4 2.2 2.2 0 0 0 0-4.4z M7.6 9.3 10.8 14 M16.4 9.3 13.2 14";
/// Protocol contract glyph (a diamond) used by topology rows.
pub const PROTO_DIAMOND: &str = "M12 3 21 12 12 21 3 12z";
/// Supervisor glyph (a shield) used by topology rows.
pub const SHIELD: &str = "M12 3 19 6v6c0 4-3 6.5-7 9-4-2.5-7-5-7-9V6z";
/// Tool / wrench glyph used by topology rows.
pub const WRENCH: &str = "M14.5 6.5a3.5 3.5 0 0 0-4.6 4.6L4 17l3 3 5.9-5.9a3.5 3.5 0 0 0 4.6-4.6l-2.2 2.2-2.1-.6-.6-2.1z";
/// Message / envelope glyph used by topology protocol-message rows.
pub const ENVELOPE: &str = "M4 7a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1z M4.5 7.5 12 13l7.5-5.5";

// ---- Status bar ----
/// Git-branch (status bar): two nodes + a branch.
pub const BRANCH: &str = "M6 3.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z M6 15.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z M18 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z M6 8.5v7M18 10.5c0 4-4 3-8 5";
/// Plus / changes glyph.
pub const PLUS: &str = "M12 3v18M3 12h18";
/// Error circle (status bar problems).
pub const ERROR_CIRCLE: &str = "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z M12 7v6M12 16h.01";
/// Warning triangle.
pub const WARN_TRI: &str = "M12 4 22 19H2z M12 10v3M12 16h.01";
/// Line-ending (LF) glyph.
pub const LF: &str = "M5 7l-3 5 3 5M19 7l3 5-3 5M14 4l-4 16";
/// Mighty language / brand mark. Kept optically centered in the 24px viewBox so
/// it reads cleanly in the rail logo, Welcome tile, status pill, and Windows ico.
pub const LANG_M: &str = "M5 18V7l7 6 7-6v11";
/// Bell / notifications.
pub const BELL: &str = "M6 9a6 6 0 0 1 12 0c0 6 2 7 2 7H4s2-1 2-7z M10 20a2 2 0 0 0 4 0";

// ---- Source control + search panels ----
/// Refresh (circular arrow) — re-run status / search.
pub const REFRESH: &str = "M20 11a8 8 0 1 0-1 5 M20 6v5h-5";
/// Commit check (checkmark) — commit affordance.
pub const CHECK: &str = "M5 12.5 10 17.5 19.5 7";
/// Stage plus (small +) — stage a row.
pub const STAGE_PLUS: &str = "M12 6v12M6 12h12";
/// Unstage minus (small -) — unstage a row.
pub const UNSTAGE_MINUS: &str = "M6 12h12";
/// Chevron-down (expanded file group in search results).
pub const CHEVRON_DOWN: &str = "M6 9l6 6 6-6";
/// Up arrow (git push — outgoing commits).
pub const ARROW_UP: &str = "M12 20V4M5 11l7-7 7 7";
/// Down arrow (git pull — incoming commits).
pub const ARROW_DOWN: &str = "M12 4v16M5 13l7 7 7-7";
/// Replace (swap arrows) — the replace field marker.
pub const REPLACE: &str = "M4 7h11l-3-3M20 17H9l3 3";

// ---- Testing (Test panel rail icon + result-row marks) ----
/// Beaker / flask (Testing rail icon): a tapered flask with a neck + a fill line.
pub const BEAKER: &str = "M9 3h6 M10 3v5L5.5 17.5A2 2 0 0 0 7.3 20.5h9.4a2 2 0 0 0 1.8-3L14 8V3 M8 13h8";
/// Red x mark (failed test row) — two crossing strokes inside the row box.
pub const XMARK: &str = "M7 7l10 10M17 7 7 17";

// ---- Command palette icons ----
/// Test workspace (terminal-ish box with a prompt).
pub const TEST_BOX: &str = "M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z M8 9.5 10.5 12 8 14.5M13 14.5h3";
/// Info circle (hover / show docs).
pub const INFO_I: &str = "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z M12 11v5M12 8h.01";
/// Quick-fix lightbulb (gutter indicator + code-action menu mark): a rounded
/// bulb glass over a short filament, with a two-line screw base beneath. Drawn
/// accent-tinted, only when code actions exist for the line.
pub const LIGHTBULB: &str = "M12 3a6 6 0 0 0-3.8 10.6c.7.6 1.1 1.2 1.2 2.1l.1.8h5l.1-.8c.1-.9.5-1.5 1.2-2.1A6 6 0 0 0 12 3z M9.5 19.5h5 M10.5 21.5h3";
/// Globe (Web Playground / Run in Browser): a circle with a meridian + two
/// latitude lines — a wasm32-web "run in the browser" glyph.
pub const GLOBE: &str = "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z M3 12h18 M12 3c2.5 2.4 3.8 5.6 3.8 9s-1.3 6.6-3.8 9c-2.5-2.4-3.8-5.6-3.8-9s1.3-6.6 3.8-9z";
