//! Transient toast notifications (shim-side, scalar-driven from Mighty).
//!
//! A small stack of self-dismissing cards in the bottom-right corner. Each toast
//! has a severity (info / success / warn / error → a colored accent + a vector
//! icon), a short message, and an age; it fades+slides in on appear and
//! fades+slides out near the end of its life, then is dropped. At most
//! [`MAX_VISIBLE`] are shown — pushing past the cap drops the oldest.
//!
//! Toasts are pushed **shim-side** for shim-originated events (file saved, git
//! committed/staged, formatted, build/run/test finished, "no definition found",
//! LSP/AI errors, theme changed, …) via [`MuiContext::push_toast`]. For
//! Mighty-originated actions, the scalar `mui_toast(kind, msg_id)` ABI maps a
//! small set of predefined message ids to strings (since strings can't cross the
//! FFI, L17).
//!
//! Per L21 all state lives here; Mighty only advances the timers
//! (`mui_toast_tick`), draws (`mui_toast_draw`), and optionally pushes a
//! predefined toast (`mui_toast`). The renderer paints on the overlay layer so
//! toasts sit above every panel/card.

use std::time::{Duration, Instant};

use crate::ffi::MuiColor;
use crate::{icons, theme};

/// Toast severity → accent color + icon + a short label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info,
    Success,
    Warn,
    Error,
}

impl Kind {
    /// Map the scalar wire value used by `mui_toast` / `push_toast`.
    pub fn from_scalar(k: i32) -> Kind {
        match k {
            1 => Kind::Success,
            2 => Kind::Warn,
            3 => Kind::Error,
            _ => Kind::Info,
        }
    }

    /// The accent color (left bar + icon) for this severity, theme-aware.
    pub fn color(self) -> MuiColor {
        match self {
            Kind::Info => theme::INFO(),
            Kind::Success => theme::GREEN(),
            Kind::Warn => theme::WARNING(),
            Kind::Error => theme::ERROR(),
        }
    }

    /// The vector icon path for this severity.
    pub fn icon(self) -> &'static str {
        match self {
            Kind::Info => icons::INFO_I,
            Kind::Success => icons::CHECK,
            Kind::Warn => icons::WARN_TRI,
            Kind::Error => icons::ERROR_CIRCLE,
        }
    }
}

/// How long each toast severity stays before it begins dismissing.
const INFO_LIFETIME: Duration = Duration::from_millis(2400);
const SUCCESS_LIFETIME: Duration = Duration::from_millis(1800);
const WARN_LIFETIME: Duration = Duration::from_millis(3600);
const ERROR_LIFETIME: Duration = Duration::from_millis(4500);
/// The fade/slide in + out animation window (each end).
const ANIM: Duration = Duration::from_millis(220);
/// Max simultaneously-visible toasts (older ones drop).
pub const MAX_VISIBLE: usize = 3;
const MARGIN: f32 = 18.0;
const RIGHT_SAFE_INSET: f32 = 96.0;
const MIN_CARD_W: f32 = 128.0;
const CARD_H: f32 = 56.0;
const GAP: f32 = 12.0;

#[allow(dead_code)]
fn toast_card_width(window_w: f32) -> f32 {
    toast_card_width_with_left(window_w, 0.0)
}

fn toast_card_width_with_left(window_w: f32, reserve_left: f32) -> f32 {
    toast_card_width_with_insets(window_w, reserve_left, 0.0)
}

fn toast_card_width_with_insets(window_w: f32, reserve_left: f32, reserve_right: f32) -> f32 {
    let left = reserve_left.max(0.0);
    let right = reserve_right.max(0.0);
    let available = window_w - left - right - MARGIN - RIGHT_SAFE_INSET;
    if available < MIN_CARD_W {
        0.0
    } else {
        320.0_f32.min(available)
    }
}

#[allow(dead_code)]
fn toast_card_x(window_w: f32, card_w: f32) -> f32 {
    toast_card_x_with_left(window_w, card_w, 0.0)
}

fn toast_card_x_with_left(window_w: f32, card_w: f32, reserve_left: f32) -> f32 {
    toast_card_x_with_insets(window_w, card_w, reserve_left, 0.0)
}

fn toast_card_x_with_insets(window_w: f32, card_w: f32, reserve_left: f32, reserve_right: f32) -> f32 {
    let right = reserve_right.max(0.0);
    (window_w - MARGIN - RIGHT_SAFE_INSET - right - card_w).max(MARGIN.max(reserve_left))
}

/// A single live toast.
#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: Kind,
    pub message: String,
    /// When the toast was pushed (drives age → fade/slide + expiry).
    born: Instant,
    /// Set when the toast has fully expired (kept for one tick so callers can
    /// observe the drop deterministically in tests via [`ToastQueue::tick`]).
    expired: bool,
}

impl Toast {
    /// Fraction `0.0..=1.0` of how opaque/settled this toast is right now (1.0
    /// fully shown; ramps up on appear, ramps down before expiry). Pure fn of the
    /// elapsed time, so the render is smooth without per-toast animation state.
    pub fn presence(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.born);
        let lifetime = self.lifetime();
        if age >= lifetime {
            return 0.0;
        }
        let anim = ANIM.as_secs_f32();
        let a = age.as_secs_f32();
        let life = lifetime.as_secs_f32();
        let fade_in = (a / anim).clamp(0.0, 1.0);
        let fade_out = ((life - a) / anim).clamp(0.0, 1.0);
        fade_in.min(fade_out)
    }

    /// True once the toast has outlived its severity-specific lifetime.
    fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.born) >= self.lifetime()
    }

    fn lifetime(&self) -> Duration {
        match self.kind {
            Kind::Info => INFO_LIFETIME,
            Kind::Success => SUCCESS_LIFETIME,
            Kind::Warn => WARN_LIFETIME,
            Kind::Error => ERROR_LIFETIME,
        }
    }
}

/// The bottom-right toast stack. Newest is pushed to the back and drawn at the
/// bottom; the stack grows upward.
#[derive(Debug, Default)]
pub struct ToastQueue {
    toasts: Vec<Toast>,
}

impl ToastQueue {
    pub fn new() -> Self {
        ToastQueue::default()
    }

    /// Push a new toast. If the queue is at [`MAX_VISIBLE`], the oldest is
    /// dropped first so the newest is always shown.
    pub fn push(&mut self, kind: Kind, message: impl Into<String>) {
        self.push_at(kind, message, Instant::now());
    }

    /// Test/seam hook: push with an explicit timestamp.
    pub fn push_at(&mut self, kind: Kind, message: impl Into<String>, now: Instant) {
        let message = sanitize_message(message.into());
        // De-dupe an identical message that is still on screen: refresh it
        // instead of stacking duplicates (e.g. repeated "Saved").
        if let Some(t) = self
            .toasts
            .iter_mut()
            .find(|t| t.kind == kind && t.message == message)
        {
            t.born = now;
            t.expired = false;
            return;
        }
        if let Some(key) = operation_key(&message) {
            self.toasts.retain(|t| operation_key(&t.message) != Some(key));
        }
        if self.toasts.len() >= MAX_VISIBLE {
            self.toasts.remove(0);
        }
        self.toasts.push(Toast {
            kind,
            message,
            born: now,
            expired: false,
        });
    }

    /// Advance timers: drop expired toasts. Returns `true` if anything changed
    /// (a toast expired) so the caller can request a redraw.
    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }

    /// Test/seam hook: advance at an explicit time.
    pub fn tick_at(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| !t.is_expired(now));
        before != self.toasts.len()
    }

    /// Clear every visible toast. Returns `true` when the stack changed so the
    /// caller can request a redraw.
    pub fn clear(&mut self) -> bool {
        let had_toasts = !self.toasts.is_empty();
        self.toasts.clear();
        had_toasts
    }

    /// Clear informational, workflow-complete toasts while preserving warnings
    /// and errors. Used when the user navigates to a different panel so stale
    /// "saved/closed/no-op" feedback does not appear to belong to the new view.
    pub fn clear_low_priority(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts
            .retain(|t| matches!(t.kind, Kind::Warn | Kind::Error));
        before != self.toasts.len()
    }

    /// Dismiss the toast under a window-space point. Returns `true` when a toast
    /// was removed. Hit-testing mirrors the draw stack so the newest/lower toast
    /// wins when cards overlap during animation.
    #[allow(dead_code)]
    pub fn dismiss_at(&mut self, width: u32, height: u32, x: f32, y: f32, now: Instant) -> bool {
        self.dismiss_at_reserved(width, height, 0.0, x, y, now)
    }

    /// Dismiss with extra bottom-reserved space, used while a lower dock is open.
    pub fn dismiss_at_reserved(
        &mut self,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        x: f32,
        y: f32,
        now: Instant,
    ) -> bool {
        self.dismiss_at_reserved_inset(width, height, reserve_bottom, 0.0, x, y, now)
    }

    /// Dismiss with bottom/left reserved space, matching the chrome-aware draw
    /// path used when the activity rail or sidebar is visible.
    pub fn dismiss_at_reserved_inset(
        &mut self,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        reserve_left: f32,
        x: f32,
        y: f32,
        now: Instant,
    ) -> bool {
        self.dismiss_at_reserved_insets(width, height, reserve_bottom, reserve_left, 0.0, x, y, now)
    }

    /// Dismiss with bottom/left/right reserved space, matching the chrome-aware
    /// draw path when side drawers are visible.
    pub fn dismiss_at_reserved_insets(
        &mut self,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        reserve_left: f32,
        reserve_right: f32,
        x: f32,
        y: f32,
        now: Instant,
    ) -> bool {
        let Some(idx) =
            self.hit_index_at(width, height, reserve_bottom, reserve_left, reserve_right, x, y, now)
        else {
            return false;
        };
        self.toasts.remove(idx);
        true
    }

    fn hit_index_at(
        &self,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        reserve_left: f32,
        reserve_right: f32,
        x: f32,
        y: f32,
        now: Instant,
    ) -> Option<usize> {
        if self.toasts.is_empty() {
            return None;
        }
        let w = width as f32;
        let h = height as f32;
        let card_w = toast_card_width_with_insets(w, reserve_left, reserve_right);
        if card_w < MIN_CARD_W {
            return None;
        }
        let bottom = toast_stack_bottom(h, reserve_bottom);
        let visible = visible_toast_count(width, height, reserve_bottom);
        for (rev, t) in self.toasts.iter().rev().take(visible).enumerate() {
            let presence = t.presence(now);
            let slot = rev as f32;
            let cy_settled = bottom - CARD_H - slot * (CARD_H + GAP);
            let cy = if presence > 0.001 {
                cy_settled + (1.0 - presence) * 16.0
            } else {
                cy_settled
            };
            let cx = toast_card_x_with_insets(w, card_w, reserve_left, reserve_right);
            if x >= cx && x <= cx + card_w && y >= cy && y <= cy + CARD_H {
                return Some(self.toasts.len() - 1 - rev);
            }
        }
        None
    }

    /// Number of currently-live toasts.
    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// Read-only view of the toasts (oldest first).
    #[allow(dead_code)]
    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Draw the bottom-right toast stack on the OVERLAY layer (over everything).
    /// No-op when empty. `now` is threaded so the render uses the same clock the
    /// tick does.
    #[allow(dead_code)]
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        self.draw_reserved(ctx, width, height, 0.0, Instant::now());
    }

    #[allow(dead_code)]
    pub fn draw_at(&self, ctx: &mut crate::MuiContext, width: u32, height: u32, now: Instant) {
        self.draw_reserved(ctx, width, height, 0.0, now);
    }

    /// Draw with extra bottom-reserved space, used while a lower dock is open.
    pub fn draw_reserved(
        &self,
        ctx: &mut crate::MuiContext,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        now: Instant,
    ) {
        self.draw_reserved_inset(ctx, width, height, reserve_bottom, 0.0, now);
    }

    /// Draw with bottom/left reserved space so toast cards do not obscure the
    /// activity rail/sidebar in compact windows.
    pub fn draw_reserved_inset(
        &self,
        ctx: &mut crate::MuiContext,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        reserve_left: f32,
        now: Instant,
    ) {
        self.draw_reserved_insets(ctx, width, height, reserve_bottom, reserve_left, 0.0, now);
    }

    /// Draw with bottom/left/right reserved space so toast cards do not obscure
    /// active drawers such as AI Copilot.
    pub fn draw_reserved_insets(
        &self,
        ctx: &mut crate::MuiContext,
        width: u32,
        height: u32,
        reserve_bottom: f32,
        reserve_left: f32,
        reserve_right: f32,
        now: Instant,
    ) {
        if self.toasts.is_empty() {
            return;
        }
        let w = width as f32;
        let h = height as f32;
        let card_w = toast_card_width_with_insets(w, reserve_left, reserve_right);
        if card_w < MIN_CARD_W {
            return;
        }
        let card_h = CARD_H;
        let gap = GAP;
        let radius = 12.0_f32;

        // Stack upward from the bottom-right, NEWEST at the bottom (last drawn).
        // Reserve a little headroom above the status bar.
        let bottom = toast_stack_bottom(h, reserve_bottom);
        let n = visible_toast_count(width, height, reserve_bottom).min(self.toasts.len());
        for (rev, t) in self.toasts.iter().rev().take(n).enumerate() {
            let presence = t.presence(now);
            if presence <= 0.001 {
                continue;
            }
            // rev 0 = newest = bottom-most.
            let slot = rev as f32;
            let cy_settled = bottom - card_h - slot * (card_h + gap);
            // Slide in from below by a few px as it appears/dismisses.
            let slide = (1.0 - presence) * 16.0;
            let cy = cy_settled + slide;
            let cx = toast_card_x_with_insets(w, card_w, reserve_left, reserve_right);
            let card_clip = Some((
                cx.max(0.0) as u32,
                cy.max(0.0) as u32,
                card_w.max(0.0) as u32,
                card_h.max(0.0) as u32,
            ));
            // Older toasts higher in the stack dim slightly so the newest reads.
            // Keep visible cards opaque enough that underlying editor/welcome
            // text cannot read through the toast during the slide animation.
            let alpha = toast_visual_alpha(presence, slot, n);
            let fill_alpha = toast_fill_alpha(presence);

            let accent = t.kind.color();

            // Shadow + elevated card + hairline border.
            ctx.dl_shadow(
                cx,
                cy + 8.0,
                card_w,
                card_h,
                radius,
                with_alpha(theme::SHADOW(), alpha * 0.9),
                30.0,
            );
            ctx.dl_round(
                cx,
                cy,
                card_w,
                card_h,
                radius,
                with_absolute_alpha(theme::ELEVATED(), fill_alpha),
            );
            ctx.dl_stroke(
                cx,
                cy,
                card_w,
                card_h,
                radius,
                with_alpha(theme::BORDER_STRONG(), alpha),
                1.0,
            );
            // Severity accent bar down the left edge (rounded).
            ctx.dl_round(cx, cy + 8.0, 3.5, card_h - 16.0, 2.0, with_alpha(accent, alpha));

            // Icon tile.
            let icon_box = cy + (card_h - 20.0) * 0.5;
            ctx.dl_round(cx + 14.0, icon_box, 24.0, 24.0, 7.0, with_alpha(accent_a(accent, 0.16), alpha));
            ctx.dl_icon(
                cx + 17.0,
                icon_box + 3.0,
                18.0,
                18.0,
                t.kind.icon(),
                with_alpha(accent, alpha),
                1.8,
                false,
            );

            // Title (severity word) + the message, wrapped/truncated to one line.
            let title = match t.kind {
                Kind::Info => "Info",
                Kind::Success => "Success",
                Kind::Warn => "Warning",
                Kind::Error => "Error",
            };
            let tx = cx + 50.0;
            ctx.text.queue_ui_sized(
                tx,
                cy + 11.0,
                title,
                with_alpha(accent, alpha),
                11.0,
                card_clip,
            );
            let msg = truncate_measured(&mut ctx.text, &t.message, card_w - 88.0, 13.0);
            ctx.text.queue_ui_sized(
                tx,
                cy + 28.0,
                &msg,
                with_alpha(theme::TEXT(), alpha),
                13.0,
                card_clip,
            );
            ctx.dl_icon(
                cx + card_w - 28.0,
                cy + 20.0,
                13.0,
                13.0,
                icons::CLOSE,
                with_alpha(theme::TEXT_3(), alpha * 0.8),
                1.6,
                false,
            );
        }
    }
}

pub(crate) fn visible_toast_count(width: u32, height: u32, reserve_bottom: f32) -> usize {
    let usable_h = height as f32 - reserve_bottom.max(0.0);
    if width <= 640 || usable_h < 520.0 {
        2
    } else {
        MAX_VISIBLE
    }
}

fn toast_stack_bottom(window_h: f32, reserve_bottom: f32) -> f32 {
    window_h - MARGIN - theme::LINE_HEIGHT() - reserve_bottom.max(0.0)
}

fn toast_visual_alpha(presence: f32, slot: f32, count: usize) -> f32 {
    if presence <= 0.001 {
        return 0.0;
    }
    let depth_dim = 1.0 - (slot / (count as f32 + 1.0)) * 0.18;
    depth_dim.clamp(0.76, 1.0)
}

fn toast_fill_alpha(presence: f32) -> f32 {
    if presence <= 0.001 { 0.0 } else { 0.99 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKey {
    Save,
    Open,
    NameInput,
    CreateFile,
    CreateFolder,
    CreateProject,
    Tab,
    Rename,
    Delete,
    Reveal,
    Copy,
    Test,
    WebRun,
    Theme,
    Diagnostic,
    CodeAction,
    Format,
    Fold,
    Replace,
    Snippet,
    History,
    MultiCursor,
    CodeIntel,
    Navigation,
    Markdown,
    Layout,
    Terminal,
    Debug,
    Git,
    Ai,
    Agents,
    Notifications,
}

fn operation_key(message: &str) -> Option<OperationKey> {
    let m = message.trim();
    if m.starts_with("AI ")
        || m.starts_with("Set ANTHROPIC_API_KEY")
        || m == "Type a message before sending"
    {
        Some(OperationKey::Ai)
    } else if m == "No notifications to clear" {
        Some(OperationKey::Notifications)
    } else if m.starts_with("Git error:")
        || m.starts_with("Switched to ")
        || m.starts_with("Created branch ")
        || m.starts_with("Pushed:")
        || m.starts_with("Pulled:")
        || m.starts_with("Fetched:")
        || m == "Staged all changes"
        || m == "Unstaged all changes"
        || m == "Committed changes"
        || m == "Nothing to stage"
        || m == "Nothing to unstage"
        || m == "Nothing to commit"
        || m == "Source control stage failed"
        || m == "Source control unstage failed"
        || m == "No source control row selected"
        || m == "Source control root missing"
        || m.starts_with("Source Control message ")
        || m == "Source Control panel closed"
        || m == "Source Control panel is already closed"
        || m.starts_with("Source control target missing")
        || m == "No hunk selected"
        || m == "Staged hunk"
        || m == "Unstaged hunk"
        || m.starts_with("Hunk apply failed:")
        || m == "No file to diff"
        || m == "No source-control row"
        || m == "No git repository for diff"
        || m.starts_with("No diff for ")
        || m == "Diff view closed"
        || m == "Diff view is already closed"
        || m == "No file to blame"
        || m.starts_with("No blame ")
        || m.starts_with("Blame on ")
        || m == "Enter a branch name"
        || m == "Branch switcher closed"
        || m == "No branch picker open"
        || m == "No branch selected"
        || m == "Not a git repository"
    {
        Some(OperationKey::Git)
    } else if m == "Open a file before running Agents"
        || m == "No agent node selected"
        || m.starts_with("Agents ")
    {
        Some(OperationKey::Agents)
    } else if m == "No unsaved files"
        || m == "Save All failed"
        || m == "Save cancelled; tab is still open"
        || m == "Unsaved changes confirmation cancelled"
        || m == "No unsaved changes confirmation open"
        || m == "Save dialog unavailable; use Save As"
        || m.starts_with("Save All cancelled")
        || m.starts_with("Save dialog unavailable")
        || m == "Use Save As to choose a file path"
        || m.starts_with("Saved ")
        || m.starts_with("Save failed")
        || m.starts_with("Auto-saved ")
        || m.ends_with(" skipped")
        || m.contains(" need Save As")
    {
        Some(OperationKey::Save)
    } else if m.starts_with("Opened folder")
        || m.starts_with("Opened file")
        || m == "Open file cancelled"
        || m == "Open folder cancelled"
        || m == "Open file dialog unavailable"
        || m == "Open folder dialog unavailable"
        || m.starts_with("Open failed")
        || m.starts_with("Recent file missing")
        || m.starts_with("Recent folder missing")
        || m == "No recent file selected"
        || m == "No recent folder selected"
    {
        Some(OperationKey::Open)
    } else if is_name_input_message(m) {
        Some(OperationKey::NameInput)
    } else if m.starts_with("Created file")
        || m == "New file cancelled"
        || m == "New file dialog unavailable"
        || m == "Choose a file inside the workspace"
        || m.starts_with("File already exists")
        || m.starts_with("File create failed")
    {
        Some(OperationKey::CreateFile)
    } else if m.starts_with("Created folder")
        || m.starts_with("Folder ready")
        || m == "New folder cancelled"
        || m == "New folder dialog unavailable"
        || m == "Choose a folder inside the workspace"
        || m.starts_with("Folder already exists")
        || m.starts_with("Folder create failed")
    {
        Some(OperationKey::CreateFolder)
    } else if m.starts_with("Created project")
        || m == "New project cancelled"
        || m == "New project dialog unavailable"
        || m.starts_with("New project failed")
        || m == "Could not create project"
        || m == "New Project needs the Mighty compiler `mty` on PATH"
        || m.starts_with("Choose an empty folder for")
        || m.starts_with("Could not prepare folder:")
        || m.starts_with("Could not inspect folder:")
        || m == "Choose a project folder name"
        || m == "Choose a parent folder"
        || m == "Choose an existing parent folder"
    {
        Some(OperationKey::CreateProject)
    } else if m == "Split editor right"
        || m == "Editor is already split"
        || m.starts_with("Focused editor pane ")
        || m == "Closed editor pane"
        || m == "Only one editor pane"
        || m == "Window minimized"
        || m == "Window maximized"
        || m == "Window restored"
        || m.starts_with("Zen mode ")
    {
        Some(OperationKey::Layout)
    } else if m == "No tab at that position"
        || m == "Tab is already first"
        || m == "Tab is already last"
        || m == "Tabs already sorted"
        || m.starts_with("Closed ")
        || m.starts_with("Reopened ")
        || m.starts_with("Duplicated ")
        || m.starts_with("Moved tab ")
        || m.starts_with("Review ")
        || m.starts_with("Reloaded ")
        || m.starts_with("Reverted ")
        || m.starts_with("Reload failed:")
        || m.starts_with("Revert failed:")
        || m == "Sorted tabs by name"
        || m == "No closed tab to reopen"
        || m == "No duplicate file tabs"
        || m == "Save or discard changes before reloading"
        || m.starts_with("No file-backed tab to ")
        || m.starts_with("No saved tabs")
        || m == "No other saved tabs to close"
    {
        Some(OperationKey::Tab)
    } else if m.starts_with("Renamed to")
        || m.starts_with("Rename failed")
        || m.starts_with("Already named")
        || m == "Rename cancelled"
        || m == "No rename input open"
        || m == "No active file to rename"
        || m == "Cannot rename this path"
    {
        Some(OperationKey::Rename)
    } else if m.starts_with("Deleted ")
        || m.starts_with("Delete failed")
        || m.starts_with("Type ")
        || m == "No active file to delete"
    {
        Some(OperationKey::Delete)
    } else if m.starts_with("Revealed ")
        || m == "No active file to reveal"
        || m == "Active file is outside Explorer root"
        || m == "Reveal in file manager is unavailable"
        || m == "Could not open file manager"
    {
        Some(OperationKey::Reveal)
    } else if m.starts_with("Copied ")
        || m == "No active file path to copy"
        || m == "No active file name to copy"
        || m == "No active file directory to copy"
        || m == "No text to copy"
        || m == "Nothing to cut"
        || m == "Could not copy text"
        || m == "Could not cut text"
        || m == "Cut selection"
        || m == "Cut line"
        || m == "Clipboard paste failed"
        || m == "Clipboard is empty"
        || m == "Pasted clipboard"
        || m == "Pasted to terminal"
        || m == "Terminal paste failed"
        || m.starts_with("Could not copy")
    {
        Some(OperationKey::Copy)
    } else if is_test_result_message(m)
        || m == "Open a Mighty file or folder before running tests"
        || m == "Open a Mighty file before running test at cursor"
        || m.starts_with("Test run failed to start:")
        || m == "No test run to stop"
        || m == "No test result row selected"
        || m == "Test result row has no file target"
        || m.starts_with("Test results ")
        || m.starts_with("Testing panel ")
        || m.starts_with("Test target missing")
    {
        Some(OperationKey::Test)
    } else if m.starts_with("Run in Browser:")
        || m.starts_with("Web ")
        || m.starts_with("No web server ")
        || m.starts_with("Opened http://")
        || m.starts_with("Opened https://")
        || m.starts_with("Run finished")
        || m.starts_with("Run failed")
        || m.starts_with("Run stopped")
        || m == "No file to run"
        || m == "No run process to stop"
        || m == "No run output row selected"
        || m == "Run output row has no file target"
        || m.starts_with("Run output ")
        || m.starts_with("Run panel ")
        || m.starts_with("Run target missing")
    {
        Some(OperationKey::WebRun)
    } else if m.starts_with("Theme:")
        || m == "Color theme picker cancelled"
        || m == "No color theme picker open"
    {
        Some(OperationKey::Theme)
    } else if is_mighty_diagnostic_message(m) {
        Some(OperationKey::Diagnostic)
    } else if m == "No code actions available"
        || m == "No code action selected"
        || m == "No code action menu open"
        || m == "Code action needs a file"
        || m == "Save failed before code action"
        || m == "Applied Fix all (mty)"
        || m == "Fix all (mty) failed"
        || m == "Applied code action"
        || m == "Code action produced no edit"
    {
        Some(OperationKey::CodeAction)
    } else if m == "Formatted document"
        || m == "Format failed"
        || m == "Save the file before formatting"
        || m == "Format is available for Mighty files"
    {
        Some(OperationKey::Format)
    } else if m == "No foldable block at cursor"
        || m == "No foldable blocks"
        || m == "All foldable blocks already folded"
        || m == "No folded blocks to unfold"
    {
        Some(OperationKey::Fold)
    } else if m == "Enter text to replace"
        || m == "Replace is unavailable in read-only previews"
        || m == "Find & Replace closed"
        || m == "No Find & Replace bar open"
        || m == "No matches to replace"
        || m == "No project replacements"
        || (m.starts_with("Replaced ") && m.contains(" occurrence"))
    {
        Some(OperationKey::Replace)
    } else if m == "Snippet session cancelled" || m == "No snippet session active" {
        Some(OperationKey::Snippet)
    } else if m == "Nothing to undo"
        || m == "Nothing to redo"
        || m == "Undo is unavailable in read-only previews"
        || m == "Redo is unavailable in read-only previews"
    {
        Some(OperationKey::History)
    } else if m == "No word or next occurrence for multi-cursor"
        || m == "No line above for another caret"
        || m == "No line below for another caret"
    {
        Some(OperationKey::MultiCursor)
    } else if m == "No completions available"
        || m == "No autocomplete suggestions open"
        || m == "Save the file before hover"
        || m == "No hover information"
        || m == "Save the file before Go to Definition"
        || m == "Save the file before Peek Definition"
        || m == "Peek view closed"
        || m == "Peek view is already closed"
        || m == "Save the file before signature help"
        || m == "No definition found"
        || m == "No definition target selected"
        || m.starts_with("Definition target missing")
        || m == "No rename target"
    {
        Some(OperationKey::CodeIntel)
    } else if m == "Breadcrumb menu closed"
        || m == "No breadcrumb menu open"
        || m == "No command palette open"
        || m == "No Quick Open panel open"
        || m == "No breadcrumb row selected"
        || m == "Breadcrumb file no longer listed"
        || m == "Breadcrumb symbol unavailable"
        || m.starts_with("Breadcrumb target missing")
        || m.starts_with("Outline symbols ")
        || m == "No search result selected"
        || m == "Search result file no longer listed"
        || m.starts_with("Search results ")
        || m.starts_with("Search target missing")
    {
        Some(OperationKey::Navigation)
    } else if m.starts_with("Markdown preview ") || m.starts_with("Markdown Preview ") {
        Some(OperationKey::Markdown)
    } else if m.starts_with("Terminal ") {
        Some(OperationKey::Terminal)
    } else if m.starts_with("Debug session ")
        || m == "Open a file before starting debug"
        || m.starts_with("Debug failed to start:")
        || m == "Run and Debug panel closed"
        || m == "Run and Debug panel is already closed"
        || m == "Continue is available when paused"
        || m == "No debug session to stop"
        || m == "Pause is available while running"
        || m == "Debug restart failed"
        || m == "No debug target to restart"
        || m == "Step Over is available when paused"
        || m == "Step Into is available when paused"
        || m == "Step Out is available when paused"
        || m == "Save the file before setting breakpoints"
    {
        Some(OperationKey::Debug)
    } else if m.starts_with("Dock ")
        || m.starts_with("Bottom dock ")
        || m.starts_with("No bottom dock ")
        || m.starts_with("Sidebar ")
        || m.starts_with("Explorer panel ")
        || m.starts_with("Problems diagnostics ")
        || m.starts_with("Problems panel ")
        || m.starts_with("Settings panel ")
        || m.starts_with("Keyboard Shortcuts ")
    {
        Some(OperationKey::Layout)
    } else {
        None
    }
}

fn is_test_result_message(message: &str) -> bool {
    if message.ends_with(" tests passed") {
        return message
            .split_whitespace()
            .next()
            .is_some_and(|count| count.chars().all(|ch| ch.is_ascii_digit()));
    }
    if !message.ends_with(" tests failed") {
        return false;
    }
    let mut words = message.split_whitespace();
    let Some(failed) = words.next() else {
        return false;
    };
    let Some(of) = words.next() else {
        return false;
    };
    let Some(total) = words.next() else {
        return false;
    };
    failed.chars().all(|ch| ch.is_ascii_digit())
        && of == "of"
        && total.chars().all(|ch| ch.is_ascii_digit())
}

fn is_mighty_diagnostic_message(message: &str) -> bool {
    let Some(rest) = message.strip_prefix("MT") else {
        return false;
    };
    let mut chars = rest.chars();
    let code: String = chars.by_ref().take(4).collect();
    code.len() == 4
        && code.chars().all(|ch| ch.is_ascii_digit())
        && chars.next() == Some(':')
}

fn is_name_input_message(message: &str) -> bool {
    message == "Enter a project name"
        || message == "No prompt input open"
        || message.starts_with("Project name too long")
        || message == "Invalid project name"
        || message == "Name must not contain path separators"
        || message == "Name must start with a letter, digit or underscore"
        || message == "Use letters, digits, '-', '_' or '.' only"
}

/// Re-alpha a color (multiplying the existing alpha by `a`).
fn with_alpha(c: MuiColor, a: f32) -> MuiColor {
    MuiColor::new(c.r, c.g, c.b, (c.a * a).clamp(0.0, 1.0))
}

/// Replace a color's alpha, used where an overlay must stay readable even in a
/// glass theme.
fn with_absolute_alpha(c: MuiColor, a: f32) -> MuiColor {
    MuiColor::new(c.r, c.g, c.b, a.clamp(0.0, 1.0))
}

/// A wash of `c` at alpha `a` (icon tile background).
fn accent_a(c: MuiColor, a: f32) -> MuiColor {
    MuiColor::new(c.r, c.g, c.b, a)
}

/// Toasts are single-line cards. Normalize multi-line command output before it
/// hits layout so old/gutter text cannot visually bleed into the next toast.
fn sanitize_message(raw: String) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for ch in raw.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        "Notification".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Truncate `s` to fit `max_px` at the UI font, appending an ellipsis.
fn truncate_measured(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    let max_px = max_px.max(0.0);
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    let ellipsis_w = text.measure_ui_sized(ellipsis, size).0;
    if ellipsis_w >= max_px {
        return ellipsis.to_string();
    }

    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let mut candidate: String = chars.iter().take(mid).collect();
        candidate.push_str(ellipsis);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }

    let mut out: String = chars.iter().take(lo).collect();
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_adds_and_reports_len() {
        let mut q = ToastQueue::new();
        assert!(q.is_empty());
        q.push(Kind::Info, "hello");
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].kind, Kind::Info);
        assert_eq!(q.toasts()[0].message, "hello");
    }

    #[test]
    fn kind_from_scalar_maps_severities() {
        assert_eq!(Kind::from_scalar(0), Kind::Info);
        assert_eq!(Kind::from_scalar(1), Kind::Success);
        assert_eq!(Kind::from_scalar(2), Kind::Warn);
        assert_eq!(Kind::from_scalar(3), Kind::Error);
        // Unknown → info.
        assert_eq!(Kind::from_scalar(99), Kind::Info);
    }

    #[test]
    fn max_visible_drops_oldest() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        // Distinct messages so the de-dupe doesn't fold them.
        for i in 0..(MAX_VISIBLE + 2) {
            q.push_at(Kind::Info, format!("msg {i}"), t0 + Duration::from_millis(i as u64));
        }
        assert_eq!(q.len(), MAX_VISIBLE);
        // Oldest two were dropped; the front is now "msg 2".
        assert_eq!(q.toasts()[0].message, "msg 2");
        assert_eq!(q.toasts().last().unwrap().message, format!("msg {}", MAX_VISIBLE + 1));
    }

    #[test]
    fn tick_expires_after_lifetime() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Success, "saved", t0);
        // Just before lifetime: still present.
        assert!(!q.tick_at(t0 + SUCCESS_LIFETIME - Duration::from_millis(1)));
        assert_eq!(q.len(), 1);
        // After lifetime: expired + dropped, tick reports a change.
        assert!(q.tick_at(t0 + SUCCESS_LIFETIME + Duration::from_millis(1)));
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn success_toasts_clear_faster_than_errors() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Success, "Created folder: sample", t0);
        q.push_at(Kind::Error, "Save failed: main.mty", t0);

        assert!(q.tick_at(t0 + SUCCESS_LIFETIME + Duration::from_millis(20)));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].kind, Kind::Error);
        assert_eq!(q.toasts()[0].message, "Save failed: main.mty");

        assert!(q.tick_at(t0 + ERROR_LIFETIME + Duration::from_millis(20)));
        assert!(q.is_empty());
    }

    #[test]
    fn presence_ramps_in_and_out() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push_at(Kind::Info, "x", t0);
        let t = &q.toasts()[0];
        // At birth: just appearing (near 0).
        assert!(t.presence(t0) < 0.2);
        // Mid-life: fully present.
        assert!((t.presence(t0 + Duration::from_millis(1500)) - 1.0).abs() < 0.05);
        // Near expiry: dismissing (< 1).
        assert!(t.presence(t0 + INFO_LIFETIME - Duration::from_millis(50)) < 0.8);
        // Past expiry: gone.
        assert_eq!(t.presence(t0 + INFO_LIFETIME + Duration::from_millis(1)), 0.0);
    }

    #[test]
    fn visible_toast_cards_stay_opaque_over_busy_content() {
        assert_eq!(toast_visual_alpha(0.0, 0.0, 3), 0.0);
        assert_eq!(toast_fill_alpha(0.0), 0.0);
        assert!(toast_visual_alpha(0.05, 0.0, 3) >= 0.98);
        assert!(toast_visual_alpha(0.05, 2.0, 3) >= 0.76);
        assert!(toast_fill_alpha(0.05) >= 0.98);
    }

    #[test]
    fn duplicate_message_refreshes_not_stacks() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Success, "Saved", t0);
        q.push_at(Kind::Success, "Saved", t0 + Duration::from_millis(500));
        // Still one toast, but its clock was refreshed (won't expire at t0+success lifetime).
        assert_eq!(q.len(), 1);
        assert!(!q.tick_at(t0 + SUCCESS_LIFETIME + Duration::from_millis(1)));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn newer_file_operation_replaces_stale_family_toast() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Error, "Save failed: main.mty", t0);
        q.push_at(Kind::Success, "Saved main.mty", t0 + Duration::from_millis(500));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Saved main.mty");
        assert_eq!(q.toasts()[0].kind, Kind::Success);

        q.push_at(Kind::Warn, "File already exists: main.mty", t0 + Duration::from_millis(600));
        q.push_at(Kind::Success, "Created file: lib.mty", t0 + Duration::from_millis(700));
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "Created file: lib.mty");
        assert!(!q
            .toasts()
            .iter()
            .any(|t| t.message == "File already exists: main.mty"));

        q.push_at(Kind::Info, "New file cancelled", t0 + Duration::from_millis(800));
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "New file cancelled");

        q.push_at(
            Kind::Warn,
            "Choose a file inside the workspace",
            t0 + Duration::from_millis(900),
        );
        q.push_at(Kind::Success, "Created file: lib.mty", t0 + Duration::from_millis(1000));
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "Created file: lib.mty");
        assert!(!q
            .toasts()
            .iter()
            .any(|t| t.message == "Choose a file inside the workspace"));

        q.push_at(
            Kind::Warn,
            "Choose a folder inside the workspace",
            t0 + Duration::from_millis(1100),
        );
        q.push_at(Kind::Success, "Created folder: src", t0 + Duration::from_millis(1200));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Created folder: src");
        assert!(!q
            .toasts()
            .iter()
            .any(|t| t.message == "Choose a folder inside the workspace"));

        q.push_at(Kind::Success, "Created project: app", t0 + Duration::from_millis(1300));
        q.push_at(Kind::Info, "New project cancelled", t0 + Duration::from_millis(1400));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "New project cancelled");
        assert!(!q.toasts().iter().any(|t| t.message == "Created project: app"));

        q.push_at(
            Kind::Warn,
            "Could not prepare folder: app",
            t0 + Duration::from_millis(1500),
        );
        q.push_at(
            Kind::Warn,
            "Could not inspect folder: app",
            t0 + Duration::from_millis(1600),
        );
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Could not inspect folder: app");
        assert!(!q
            .toasts()
            .iter()
            .any(|t| t.message == "Could not prepare folder: app"));

        q.push_at(Kind::Info, "Closed 1 saved tab", t0 + Duration::from_millis(1700));
        q.push_at(Kind::Warn, "No tab at that position", t0 + Duration::from_millis(1800));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "No tab at that position");
        assert!(!q.toasts().iter().any(|t| t.message == "Closed 1 saved tab"));

        q.push_at(Kind::Info, "Tab is already first", t0 + Duration::from_millis(1900));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Tab is already first");
        assert!(!q.toasts().iter().any(|t| t.message == "No tab at that position"));

        q.push_at(Kind::Info, "Tabs already sorted", t0 + Duration::from_millis(2000));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Tabs already sorted");
        assert!(!q.toasts().iter().any(|t| t.message == "Tab is already first"));

        q.push_at(
            Kind::Warn,
            "Review unsaved changes in main.mty",
            t0 + Duration::from_millis(2100),
        );
        q.push_at(
            Kind::Warn,
            "Save or discard changes before reloading",
            t0 + Duration::from_millis(2200),
        );
        assert_eq!(q.len(), 3);
        assert_eq!(
            q.toasts()[2].message,
            "Save or discard changes before reloading"
        );
        assert!(!q
            .toasts()
            .iter()
            .any(|t| t.message == "Review unsaved changes in main.mty"));

        q.push_at(Kind::Info, "Reloaded main.mty", t0 + Duration::from_millis(2300));
        q.push_at(Kind::Info, "Reverted main.mty", t0 + Duration::from_millis(2400));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Reverted main.mty");
        assert!(!q.toasts().iter().any(|t| t.message == "Reloaded main.mty"));

        q.push_at(
            Kind::Error,
            "Reload failed: main.mty",
            t0 + Duration::from_millis(2500),
        );
        q.push_at(
            Kind::Info,
            "No file-backed tab to reload",
            t0 + Duration::from_millis(2600),
        );
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "No file-backed tab to reload");
        assert!(!q.toasts().iter().any(|t| t.message == "Reload failed: main.mty"));

        q.push_at(Kind::Info, "Dock resized to 228px", t0 + Duration::from_millis(2700));
        q.push_at(Kind::Info, "Sidebar resized to 310px", t0 + Duration::from_millis(2800));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Sidebar resized to 310px");
        assert!(!q.toasts().iter().any(|t| t.message == "Dock resized to 228px"));

        q.push_at(Kind::Info, "Problems panel closed", t0 + Duration::from_millis(2850));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Problems panel closed");
        assert!(!q.toasts().iter().any(|t| t.message == "Sidebar resized to 310px"));

        q.push_at(Kind::Info, "Markdown preview opened", t0 + Duration::from_millis(2900));
        q.push_at(Kind::Info, "Markdown preview closed", t0 + Duration::from_millis(3000));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Markdown preview closed");
        assert!(!q.toasts().iter().any(|t| t.message == "Markdown preview opened"));

        q.push_at(
            Kind::Info,
            "Markdown preview is already closed",
            t0 + Duration::from_millis(3100),
        );
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Markdown preview is already closed");
        assert!(!q.toasts().iter().any(|t| t.message == "Markdown preview closed"));
    }

    #[test]
    fn newer_clipboard_feedback_replaces_stale_copy_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Success, "Copied selection", t0);
        q.push_at(Kind::Success, "Cut line", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Cut line");

        q.push_at(Kind::Info, "Clipboard is empty", t0 + Duration::from_millis(200));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Clipboard is empty");

        q.push_at(Kind::Success, "Pasted clipboard", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Pasted clipboard");
        assert_eq!(q.toasts()[0].kind, Kind::Success);

        q.push_at(Kind::Error, "Terminal paste failed", t0 + Duration::from_millis(400));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Terminal paste failed");
        assert_eq!(q.toasts()[0].kind, Kind::Error);

        q.push_at(Kind::Success, "Pasted to terminal", t0 + Duration::from_millis(500));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Pasted to terminal");
        assert_eq!(q.toasts()[0].kind, Kind::Success);
    }

    #[test]
    fn newer_save_dialog_outcomes_replace_stale_save_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Error, "Save failed: main.mty", t0);
        q.push_at(
            Kind::Info,
            "Save cancelled; tab is still open",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Save cancelled; tab is still open");

        q.push_at(
            Kind::Warn,
            "Save dialog unavailable; use Save As",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Save dialog unavailable; use Save As");

        q.push_at(
            Kind::Info,
            "Save All cancelled; 1 untitled file still unsaved",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Save All cancelled; 1 untitled file still unsaved"
        );

        q.push_at(
            Kind::Info,
            "Unsaved changes confirmation cancelled",
            t0 + Duration::from_millis(350),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Unsaved changes confirmation cancelled"
        );

        q.push_at(
            Kind::Info,
            "No unsaved changes confirmation open",
            t0 + Duration::from_millis(375),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "No unsaved changes confirmation open"
        );

        q.push_at(Kind::Warn, "Use Save As to choose a file path", t0 + Duration::from_millis(400));
        q.push_at(Kind::Success, "Saved 2 files", t0 + Duration::from_millis(400));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Saved 2 files");
    }

    #[test]
    fn newer_open_dialog_outcomes_replace_stale_open_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Success, "Opened file: main.mty", t0);
        q.push_at(Kind::Info, "Open file cancelled", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Open file cancelled");

        q.push_at(Kind::Success, "Opened folder: mighty-ide", t0 + Duration::from_millis(200));
        q.push_at(Kind::Info, "Open folder cancelled", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Open folder cancelled");

        q.push_at(
            Kind::Warn,
            "Open folder dialog unavailable",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Open folder dialog unavailable");
    }

    #[test]
    fn newer_name_input_feedback_replaces_stale_validation_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "Enter a project name", t0);
        q.push_at(
            Kind::Warn,
            "Name must not contain path separators",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Name must not contain path separators");

        q.push_at(
            Kind::Warn,
            "Name must start with a letter, digit or underscore",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Name must start with a letter, digit or underscore"
        );

        q.push_at(
            Kind::Warn,
            "Use letters, digits, '-', '_' or '.' only",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Use letters, digits, '-', '_' or '.' only"
        );

        q.push_at(
            Kind::Info,
            "No prompt input open",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No prompt input open");
    }

    #[test]
    fn newer_result_operation_replaces_stale_family_toast() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Error, "2 of 8 tests failed", t0);
        q.push_at(Kind::Success, "8 tests passed", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "8 tests passed");
        assert_eq!(q.toasts()[0].kind, Kind::Success);

        q.push_at(Kind::Info, "Run in Browser: mty serve...", t0 + Duration::from_millis(200));
        q.push_at(
            Kind::Error,
            "Run in Browser: build failed (see panel)",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "Run in Browser: build failed (see panel)");
        assert!(!q
            .toasts()
            .iter()
            .any(|t| t.message == "Run in Browser: mty serve..."));

        q.push_at(Kind::Warn, "Web URL not ready", t0 + Duration::from_millis(350));
        q.push_at(
            Kind::Info,
            "No web server running",
            t0 + Duration::from_millis(360),
        );
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "No web server running");
        assert!(!q.toasts().iter().any(|t| t.message == "Web URL not ready"));

        q.push_at(Kind::Error, "Format failed", t0 + Duration::from_millis(400));
        q.push_at(Kind::Success, "Formatted document", t0 + Duration::from_millis(500));
        assert_eq!(q.len(), 3);
        assert_eq!(q.toasts()[2].message, "Formatted document");
        assert!(!q.toasts().iter().any(|t| t.message == "Format failed"));
    }

    #[test]
    fn newer_test_feedback_replaces_stale_test_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(
            Kind::Warn,
            "Open a Mighty file or folder before running tests",
            t0,
        );
        q.push_at(
            Kind::Warn,
            "Open a Mighty file before running test at cursor",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Open a Mighty file before running test at cursor"
        );

        q.push_at(
            Kind::Error,
            "Test run failed to start: main.mty",
            t0 + Duration::from_millis(200),
        );
        q.push_at(
            Kind::Info,
            "No test run to stop",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No test run to stop");

        q.push_at(Kind::Error, "1 of 3 tests failed", t0 + Duration::from_millis(400));
        q.push_at(Kind::Success, "3 tests passed", t0 + Duration::from_millis(500));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "3 tests passed");
        assert_eq!(q.toasts()[0].kind, Kind::Success);
    }

    #[test]
    fn newer_visual_and_diagnostic_feedback_replaces_stale_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Theme: Aurora Glass", t0);
        q.push_at(Kind::Info, "Theme: Vivid Modern", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Theme: Vivid Modern");

        q.push_at(
            Kind::Info,
            "Color theme picker cancelled",
            t0 + Duration::from_millis(150),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Color theme picker cancelled");

        q.push_at(
            Kind::Info,
            "No color theme picker open",
            t0 + Duration::from_millis(175),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No color theme picker open");

        q.push_at(Kind::Error, "MT1001: expected I32", t0 + Duration::from_millis(200));
        q.push_at(Kind::Error, "MT2001: expected I32, found Str", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "MT2001: expected I32, found Str");
        assert!(!q.toasts().iter().any(|t| t.message == "MT1001: expected I32"));
    }

    #[test]
    fn newer_pane_lifecycle_feedback_replaces_stale_layout_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Split editor right", t0);
        q.push_at(
            Kind::Info,
            "Focused editor pane 2",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Focused editor pane 2");

        q.push_at(Kind::Info, "Closed editor pane", t0 + Duration::from_millis(200));
        q.push_at(Kind::Info, "Only one editor pane", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Only one editor pane");

        q.push_at(Kind::Info, "Window minimized", t0 + Duration::from_millis(400));
        q.push_at(Kind::Info, "Window maximized", t0 + Duration::from_millis(500));
        q.push_at(Kind::Info, "Window restored", t0 + Duration::from_millis(600));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Window restored");

        q.push_at(
            Kind::Info,
            "Zen mode on \u{2014} Alt+Z to exit",
            t0 + Duration::from_millis(700),
        );
        q.push_at(Kind::Info, "Zen mode off", t0 + Duration::from_millis(800));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Zen mode off");

        q.push_at(Kind::Info, "Settings panel closed", t0 + Duration::from_millis(900));
        q.push_at(
            Kind::Info,
            "Settings panel is already closed",
            t0 + Duration::from_millis(1000),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Settings panel is already closed");
        assert!(!q.toasts().iter().any(|t| t.message == "Settings panel closed"));

        q.push_at(
            Kind::Info,
            "Keyboard Shortcuts closed",
            t0 + Duration::from_millis(1100),
        );
        q.push_at(
            Kind::Info,
            "Keyboard Shortcuts is already closed",
            t0 + Duration::from_millis(1200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Keyboard Shortcuts is already closed");
        assert!(!q.toasts().iter().any(|t| t.message == "Keyboard Shortcuts closed"));
    }

    #[test]
    fn newer_git_feedback_replaces_stale_hunk_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "No hunk selected", t0);
        q.push_at(Kind::Success, "Staged hunk", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Staged hunk");
        assert_eq!(q.toasts()[0].kind, Kind::Success);

        q.push_at(
            Kind::Error,
            "Hunk apply failed: patch does not apply",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Hunk apply failed: patch does not apply");
        assert_eq!(q.toasts()[0].kind, Kind::Error);

        q.push_at(Kind::Warn, "Not a git repository", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Not a git repository");

        q.push_at(
            Kind::Info,
            "Branch switcher closed",
            t0 + Duration::from_millis(400),
        );
        q.push_at(
            Kind::Info,
            "No branch picker open",
            t0 + Duration::from_millis(500),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No branch picker open");
        assert!(!q.toasts().iter().any(|t| t.message == "Branch switcher closed"));
    }

    #[test]
    fn newer_git_feedback_replaces_stale_diff_open_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "No file to diff", t0);
        q.push_at(
            Kind::Warn,
            "No git repository for diff",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No git repository for diff");

        q.push_at(
            Kind::Info,
            "No diff for main.mty",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No diff for main.mty");
        assert_eq!(q.toasts()[0].kind, Kind::Info);

        q.push_at(Kind::Info, "Diff view closed", t0 + Duration::from_millis(250));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Diff view closed");

        q.push_at(
            Kind::Info,
            "Diff view is already closed",
            t0 + Duration::from_millis(275),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Diff view is already closed");

        q.push_at(
            Kind::Warn,
            "No source-control row",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No source-control row");
    }

    #[test]
    fn newer_git_feedback_replaces_stale_source_control_row_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "No source control row selected", t0);
        q.push_at(
            Kind::Warn,
            "Source control root missing",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Source control root missing");
        assert_eq!(q.toasts()[0].kind, Kind::Warn);

        q.push_at(
            Kind::Warn,
            "Source control target missing: deleted.mty",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Source control target missing: deleted.mty"
        );

        q.push_at(
            Kind::Success,
            "Staged all changes",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Staged all changes");
    }

    #[test]
    fn newer_git_feedback_replaces_stale_blame_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "No file to blame", t0);
        q.push_at(
            Kind::Warn,
            "No blame (file not tracked?)",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No blame (file not tracked?)");

        q.push_at(
            Kind::Info,
            "Blame on \u{2014} toggle to hide",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Blame on \u{2014} toggle to hide");
        assert_eq!(q.toasts()[0].kind, Kind::Info);

        q.push_at(Kind::Info, "Nothing to commit", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Nothing to commit");
    }

    #[test]
    fn newer_terminal_feedback_replaces_stale_terminal_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Terminal opened", t0);
        q.push_at(Kind::Info, "Terminal closed", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Terminal closed");

        q.push_at(
            Kind::Info,
            "Terminal is already closed",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Terminal is already closed");

        q.push_at(Kind::Info, "Terminal cleared", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Terminal cleared");

        q.push_at(
            Kind::Error,
            "Terminal failed to open",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Terminal failed to open");
        assert_eq!(q.toasts()[0].kind, Kind::Error);
    }

    #[test]
    fn newer_debug_feedback_replaces_stale_debug_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "Open a file before starting debug", t0);
        q.push_at(
            Kind::Info,
            "Debug session already running",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Debug session already running");

        q.push_at(
            Kind::Info,
            "Continue is available when paused",
            t0 + Duration::from_millis(200),
        );
        q.push_at(
            Kind::Info,
            "Step Over is available when paused",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Step Over is available when paused");

        q.push_at(
            Kind::Info,
            "Step Into is available when paused",
            t0 + Duration::from_millis(400),
        );
        q.push_at(
            Kind::Info,
            "Step Out is available when paused",
            t0 + Duration::from_millis(500),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Step Out is available when paused");

        q.push_at(
            Kind::Error,
            "Debug restart failed",
            t0 + Duration::from_millis(600),
        );
        q.push_at(
            Kind::Warn,
            "Save the file before setting breakpoints",
            t0 + Duration::from_millis(700),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Save the file before setting breakpoints"
        );
        assert_eq!(q.toasts()[0].kind, Kind::Warn);
    }

    #[test]
    fn newer_run_feedback_replaces_stale_run_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Run output cleared", t0);
        q.push_at(
            Kind::Info,
            "Run output already empty",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Run output already empty");

        q.push_at(
            Kind::Info,
            "Run panel closed",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Run panel closed");

        q.push_at(
            Kind::Info,
            "Run panel is already closed",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Run panel is already closed");
    }

    #[test]
    fn newer_testing_feedback_replaces_stale_testing_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Test results cleared", t0);
        q.push_at(
            Kind::Info,
            "Test results already empty",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Test results already empty");

        q.push_at(
            Kind::Info,
            "Testing panel closed",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Testing panel closed");

        q.push_at(
            Kind::Info,
            "Testing panel is already closed",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Testing panel is already closed");
    }

    #[test]
    fn newer_fold_feedback_replaces_stale_fold_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "No foldable block at cursor", t0);
        q.push_at(
            Kind::Info,
            "No foldable blocks",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No foldable blocks");

        q.push_at(
            Kind::Info,
            "All foldable blocks already folded",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "All foldable blocks already folded"
        );

        q.push_at(
            Kind::Info,
            "No folded blocks to unfold",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No folded blocks to unfold");
    }

    #[test]
    fn newer_replace_feedback_replaces_stale_replace_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Enter text to replace", t0);
        q.push_at(
            Kind::Warn,
            "Replace is unavailable in read-only previews",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Replace is unavailable in read-only previews"
        );
        assert_eq!(q.toasts()[0].kind, Kind::Warn);

        q.push_at(
            Kind::Success,
            "Replaced 2 occurrences",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Replaced 2 occurrences");

        q.push_at(
            Kind::Warn,
            "Replaced 1 occurrence; 1 dirty open tab not refreshed",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Replaced 1 occurrence; 1 dirty open tab not refreshed"
        );

        q.push_at(
            Kind::Warn,
            "No project replacements",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No project replacements");

        q.push_at(
            Kind::Info,
            "Find & Replace closed",
            t0 + Duration::from_millis(500),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Find & Replace closed");

        q.push_at(
            Kind::Info,
            "No Find & Replace bar open",
            t0 + Duration::from_millis(600),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No Find & Replace bar open");
    }

    #[test]
    fn newer_history_feedback_replaces_stale_undo_redo_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Nothing to undo", t0);
        q.push_at(Kind::Info, "Nothing to redo", t0 + Duration::from_millis(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Nothing to redo");

        q.push_at(
            Kind::Warn,
            "Undo is unavailable in read-only previews",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Undo is unavailable in read-only previews"
        );
        assert_eq!(q.toasts()[0].kind, Kind::Warn);

        q.push_at(
            Kind::Warn,
            "Redo is unavailable in read-only previews",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Redo is unavailable in read-only previews"
        );
    }

    #[test]
    fn newer_snippet_feedback_replaces_stale_session_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Snippet session cancelled", t0);
        q.push_at(
            Kind::Info,
            "No snippet session active",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No snippet session active");

        q.push_at(
            Kind::Info,
            "Snippet session cancelled",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Snippet session cancelled");
    }

    #[test]
    fn newer_multi_cursor_feedback_replaces_stale_multi_cursor_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "No word or next occurrence for multi-cursor", t0);
        q.push_at(
            Kind::Info,
            "No line above for another caret",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No line above for another caret");

        q.push_at(
            Kind::Info,
            "No line below for another caret",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No line below for another caret");
    }

    #[test]
    fn newer_code_intelligence_feedback_replaces_stale_language_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "No completions available", t0);
        q.push_at(
            Kind::Warn,
            "Save the file before hover",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Save the file before hover");
        assert_eq!(q.toasts()[0].kind, Kind::Warn);

        q.push_at(
            Kind::Info,
            "No autocomplete suggestions open",
            t0 + Duration::from_millis(150),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No autocomplete suggestions open");

        q.push_at(
            Kind::Info,
            "No hover information",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No hover information");

        q.push_at(
            Kind::Warn,
            "Save the file before Go to Definition",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Save the file before Go to Definition");

        q.push_at(
            Kind::Warn,
            "Definition target missing: missing.mty",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Definition target missing: missing.mty");

        q.push_at(Kind::Info, "Peek view closed", t0 + Duration::from_millis(450));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Peek view closed");

        q.push_at(
            Kind::Info,
            "Peek view is already closed",
            t0 + Duration::from_millis(475),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Peek view is already closed");

        q.push_at(
            Kind::Warn,
            "Save the file before signature help",
            t0 + Duration::from_millis(500),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Save the file before signature help");

        q.push_at(
            Kind::Info,
            "No rename target",
            t0 + Duration::from_millis(600),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No rename target");

        q.push_at(Kind::Info, "Rename cancelled", t0 + Duration::from_millis(700));
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "Rename cancelled");

        q.push_at(
            Kind::Info,
            "No rename input open",
            t0 + Duration::from_millis(725),
        );
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[1].message, "No rename input open");
    }

    #[test]
    fn newer_code_action_feedback_replaces_stale_menu_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "No code actions available", t0);
        q.push_at(
            Kind::Info,
            "No code action menu open",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No code action menu open");

        q.push_at(
            Kind::Warn,
            "Code action needs a file",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Code action needs a file");

        q.push_at(Kind::Success, "Applied code action", t0 + Duration::from_millis(300));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Applied code action");
    }

    #[test]
    fn newer_navigation_feedback_replaces_stale_breadcrumb_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Breadcrumb menu closed", t0);
        q.push_at(
            Kind::Info,
            "No breadcrumb menu open",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No breadcrumb menu open");

        q.push_at(
            Kind::Info,
            "No breadcrumb row selected",
            t0 + Duration::from_millis(200),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No breadcrumb row selected");

        q.push_at(
            Kind::Info,
            "No command palette open",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No command palette open");

        q.push_at(
            Kind::Info,
            "No Quick Open panel open",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No Quick Open panel open");
    }

    #[test]
    fn newer_format_feedback_replaces_stale_format_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "Save the file before formatting", t0);
        q.push_at(
            Kind::Info,
            "Format is available for Mighty files",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Format is available for Mighty files"
        );
        assert_eq!(q.toasts()[0].kind, Kind::Info);

        q.push_at(Kind::Error, "Format failed", t0 + Duration::from_millis(200));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Format failed");
        assert_eq!(q.toasts()[0].kind, Kind::Error);

        q.push_at(
            Kind::Success,
            "Formatted document",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Formatted document");
    }

    #[test]
    fn newer_ai_feedback_replaces_stale_ai_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "Type a message before sending", t0);
        q.push_at(
            Kind::Warn,
            "Set ANTHROPIC_API_KEY to enable AI Copilot",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Set ANTHROPIC_API_KEY to enable AI Copilot"
        );

        q.push_at(Kind::Info, "AI Copilot closed", t0 + Duration::from_millis(200));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "AI Copilot closed");

        q.push_at(
            Kind::Warn,
            "AI inline completion is disabled in Settings",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "AI inline completion is disabled in Settings"
        );

        q.push_at(
            Kind::Warn,
            "Set ANTHROPIC_API_KEY to enable Inline AI",
            t0 + Duration::from_millis(400),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.toasts()[0].message,
            "Set ANTHROPIC_API_KEY to enable Inline AI"
        );
    }

    #[test]
    fn newer_agents_feedback_replaces_stale_agents_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Warn, "Open a file before running Agents", t0);
        q.push_at(
            Kind::Info,
            "No agent node selected",
            t0 + Duration::from_millis(100),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No agent node selected");

        q.push_at(
            Kind::Info,
            "Agents node has no file target",
            t0 + Duration::from_millis(200),
        );
        q.push_at(
            Kind::Warn,
            "Agents target missing: agent.mty",
            t0 + Duration::from_millis(300),
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "Agents target missing: agent.mty");
        assert_eq!(q.toasts()[0].kind, Kind::Warn);
    }

    #[test]
    fn newer_notification_feedback_replaces_stale_notification_toasts() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();

        q.push_at(Kind::Info, "No notifications to clear", t0);
        q.push_at(
            Kind::Info,
            "No notifications to clear",
            t0 + Duration::from_millis(100),
        );

        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "No notifications to clear");
    }

    #[test]
    fn push_sanitizes_multiline_messages_for_single_line_cards() {
        let mut q = ToastQueue::new();
        q.push(
            Kind::Error,
            "Save failed:\r\nC:\\tmp\\main.mty\tpermission denied\u{0007}",
        );
        assert_eq!(
            q.toasts()[0].message,
            "Save failed: C:\\tmp\\main.mty permission denied"
        );
    }

    #[test]
    fn blank_messages_fall_back_to_a_stable_label() {
        let mut q = ToastQueue::new();
        q.push(Kind::Info, "\r\n\t");
        assert_eq!(q.toasts()[0].message, "Notification");
    }

    #[test]
    fn clear_drops_all_visible_toasts_and_reports_change() {
        let mut q = ToastQueue::new();
        assert!(!q.clear());
        q.push(Kind::Info, "one");
        q.push(Kind::Warn, "two");
        assert_eq!(q.len(), 2);
        assert!(q.clear());
        assert!(q.is_empty());
        assert!(!q.clear());
    }

    #[test]
    fn clear_low_priority_keeps_attention_toasts() {
        let mut q = ToastQueue::new();
        q.push(Kind::Info, "No debug session to stop");
        q.push(Kind::Success, "Saved main.mty");
        q.push(Kind::Warn, "Save or discard changes before reloading");
        q.push(Kind::Error, "Save failed: main.mty");

        assert!(q.clear_low_priority());
        assert_eq!(q.len(), 2);
        assert_eq!(q.toasts()[0].kind, Kind::Warn);
        assert_eq!(q.toasts()[1].kind, Kind::Error);
        assert!(!q.clear_low_priority());
    }

    #[test]
    fn dismiss_at_removes_clicked_toast_only() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Info, "top", t0);
        q.push_at(Kind::Warn, "bottom", t0);
        assert!(!q.dismiss_at(900, 600, 10.0, 10.0, t0 + Duration::from_millis(500)));

        // Newest toast is bottom-most and inset from the right edge.
        let w = 900.0;
        let h = 600.0;
        let cw = toast_card_width(w);
        let cx = toast_card_x(w, cw);
        let bottom = h - MARGIN - theme::LINE_HEIGHT();
        let cy = bottom - CARD_H;
        assert!(q.dismiss_at(
            900,
            600,
            cx + cw - 20.0,
            cy + 24.0,
            t0 + Duration::from_millis(500)
        ));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "top");
    }

    #[test]
    fn reserved_bottom_moves_toast_hit_target_above_lower_dock() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Success, "Saved", t0);

        let w = 900.0;
        let h = 600.0;
        let reserve = 180.0;
        let cw = toast_card_width(w);
        let cx = toast_card_x(w, cw);
        let cy = toast_stack_bottom(h, reserve) - CARD_H;

        assert!(!q.dismiss_at_reserved(
            900,
            600,
            reserve,
            cx + cw - 20.0,
            h - MARGIN - theme::LINE_HEIGHT() - 20.0,
            t0 + Duration::from_millis(500)
        ));
        assert_eq!(q.len(), 1);
        assert!(q.dismiss_at_reserved(
            900,
            600,
            reserve,
            cx + cw - 20.0,
            cy + 24.0,
            t0 + Duration::from_millis(500)
        ));
        assert!(q.is_empty());
    }

    #[test]
    fn reserved_left_keeps_compact_toasts_out_of_sidebar() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Info, "Theme: Vivid Modern", t0);

        let w = 520.0;
        let h = 360.0;
        let reserve_left = crate::layout::RAIL_W + crate::layout::SIDEBAR_MIN_W + 10.0;
        let cw = toast_card_width_with_left(w, reserve_left);
        let cx = toast_card_x_with_left(w, cw, reserve_left);
        let cy = toast_stack_bottom(h, 0.0) - CARD_H;

        assert!(cx >= reserve_left);
        assert!(cw < toast_card_width(w));
        assert!(!q.dismiss_at_reserved_inset(
            520,
            360,
            0.0,
            reserve_left,
            reserve_left - 8.0,
            cy + 24.0,
            t0 + Duration::from_millis(500)
        ));
        assert!(q.dismiss_at_reserved_inset(
            520,
            360,
            0.0,
            reserve_left,
            cx + cw - 20.0,
            cy + 24.0,
            t0 + Duration::from_millis(500)
        ));
        assert!(q.is_empty());
    }

    #[test]
    fn reserved_lanes_shrink_toasts_without_crossing_chrome() {
        let w = 520.0;
        let reserve_left = 270.0;
        let reserve_right = 0.0;
        let cw = toast_card_width_with_insets(w, reserve_left, reserve_right);
        let cx = toast_card_x_with_insets(w, cw, reserve_left, reserve_right);

        assert!(cw >= MIN_CARD_W);
        assert!(cw < 180.0, "compact toasts should shrink instead of forcing the old minimum");
        assert!(cx >= reserve_left);
        assert!(
            cx + cw <= w - reserve_right - MARGIN - RIGHT_SAFE_INSET + 0.5,
            "toast right edge must stay inside its safe lane: x={cx} w={cw}"
        );
    }

    #[test]
    fn over_reserved_lanes_hide_toasts_from_draw_and_hit_testing() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Warn, "Set ANTHROPIC_API_KEY to enable AI Copilot", t0);

        let w = 520.0;
        let h = 360.0;
        let reserve_left = 320.0;
        let reserve_right = 30.0;
        assert_eq!(toast_card_width_with_insets(w, reserve_left, reserve_right), 0.0);
        assert!(!q.dismiss_at_reserved_insets(
            w as u32,
            h as u32,
            0.0,
            reserve_left,
            reserve_right,
            reserve_left + 24.0,
            h - 70.0,
            t0 + Duration::from_millis(500)
        ));
        assert_eq!(q.len(), 1, "hidden toasts should remain queued until space returns or they expire");
    }

    #[test]
    fn reserved_right_keeps_toasts_out_of_ai_drawer() {
        let w = 1280.0;
        let reserve_left = crate::layout::RAIL_W + crate::layout::SIDEBAR_W + 10.0;
        let reserve_right = crate::ai::AI_PANEL_W + 10.0;
        let cw = toast_card_width_with_insets(w, reserve_left, reserve_right);
        let cx = toast_card_x_with_insets(w, cw, reserve_left, reserve_right);

        assert!(cx >= reserve_left);
        assert!(
            cx + cw <= w - reserve_right - RIGHT_SAFE_INSET,
            "toast right edge must stay left of the AI drawer: x={cx} w={cw}"
        );
    }

    #[test]
    fn reserved_right_hit_testing_matches_shifted_toast() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Warn, "Set ANTHROPIC_API_KEY to enable AI Copilot", t0);

        let w = 1280.0;
        let h = 832.0;
        let reserve_left = crate::layout::RAIL_W + crate::layout::SIDEBAR_W + 10.0;
        let reserve_right = crate::ai::AI_PANEL_W + 10.0;
        let cw = toast_card_width_with_insets(w, reserve_left, reserve_right);
        let cx = toast_card_x_with_insets(w, cw, reserve_left, reserve_right);
        let cy = toast_stack_bottom(h, 0.0) - CARD_H;

        assert!(!q.dismiss_at_reserved_insets(
            w as u32,
            h as u32,
            0.0,
            reserve_left,
            reserve_right,
            w - reserve_right + 24.0,
            cy + 24.0,
            t0 + Duration::from_millis(500)
        ));
        assert_eq!(q.len(), 1);
        assert!(q.dismiss_at_reserved_insets(
            w as u32,
            h as u32,
            0.0,
            reserve_left,
            reserve_right,
            cx + cw - 20.0,
            cy + 24.0,
            t0 + Duration::from_millis(500)
        ));
        assert!(q.is_empty());
    }
}
