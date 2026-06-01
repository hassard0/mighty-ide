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

/// How long a toast stays before it begins dismissing.
const LIFETIME: Duration = Duration::from_millis(3000);
/// The fade/slide in + out animation window (each end).
const ANIM: Duration = Duration::from_millis(220);
/// Max simultaneously-visible toasts (older ones drop).
pub const MAX_VISIBLE: usize = 4;

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
        if age >= LIFETIME {
            return 0.0;
        }
        let anim = ANIM.as_secs_f32();
        let a = age.as_secs_f32();
        let life = LIFETIME.as_secs_f32();
        let fade_in = (a / anim).clamp(0.0, 1.0);
        let fade_out = ((life - a) / anim).clamp(0.0, 1.0);
        fade_in.min(fade_out)
    }

    /// True once the toast has outlived [`LIFETIME`] and should be removed.
    fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.born) >= LIFETIME
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

    /// Dismiss the toast under a window-space point. Returns `true` when a toast
    /// was removed. Hit-testing mirrors the draw stack so the newest/lower toast
    /// wins when cards overlap during animation.
    pub fn dismiss_at(&mut self, width: u32, height: u32, x: f32, y: f32, now: Instant) -> bool {
        let Some(idx) = self.hit_index_at(width, height, x, y, now) else {
            return false;
        };
        self.toasts.remove(idx);
        true
    }

    fn hit_index_at(&self, width: u32, height: u32, x: f32, y: f32, now: Instant) -> Option<usize> {
        if self.toasts.is_empty() {
            return None;
        }
        let w = width as f32;
        let h = height as f32;
        let margin = 18.0_f32;
        let card_w = 320.0_f32.min(w - 2.0 * margin);
        let card_h = 56.0_f32;
        let gap = 12.0_f32;
        let bottom = h - margin - theme::LINE_HEIGHT();
        for (rev, t) in self.toasts.iter().rev().enumerate() {
            let presence = t.presence(now);
            let slot = rev as f32;
            let cy_settled = bottom - card_h - slot * (card_h + gap);
            let cy = if presence > 0.001 {
                cy_settled + (1.0 - presence) * 16.0
            } else {
                cy_settled
            };
            let cx = w - margin - card_w;
            if x >= cx && x <= cx + card_w && y >= cy && y <= cy + card_h {
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
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        self.draw_at(ctx, width, height, Instant::now());
    }

    pub fn draw_at(&self, ctx: &mut crate::MuiContext, width: u32, height: u32, now: Instant) {
        if self.toasts.is_empty() {
            return;
        }
        let w = width as f32;
        let h = height as f32;
        let margin = 18.0_f32;
        let card_w = 320.0_f32.min(w - 2.0 * margin);
        let card_h = 56.0_f32;
        let gap = 12.0_f32;
        let radius = 12.0_f32;

        // Stack upward from the bottom-right, NEWEST at the bottom (last drawn).
        // Reserve a little headroom above the status bar.
        let bottom = h - margin - theme::LINE_HEIGHT();
        let n = self.toasts.len();
        for (rev, t) in self.toasts.iter().rev().enumerate() {
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
            let cx = w - margin - card_w;
            let card_clip = Some((
                cx.max(0.0) as u32,
                cy.max(0.0) as u32,
                card_w.max(0.0) as u32,
                card_h.max(0.0) as u32,
            ));
            // Older toasts higher in the stack dim slightly so the newest reads.
            let depth_dim = 1.0 - (slot / (n as f32 + 1.0)) * 0.18;
            let alpha = presence * depth_dim;

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
            ctx.dl_round(cx, cy, card_w, card_h, radius, with_alpha(theme::ELEVATED(), alpha));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKey {
    Save,
    Open,
    CreateFile,
    CreateFolder,
    Rename,
    Delete,
    Reveal,
    Copy,
}

fn operation_key(message: &str) -> Option<OperationKey> {
    let m = message.trim();
    if m == "No unsaved files"
        || m == "Save All failed"
        || m.starts_with("Saved ")
        || m.starts_with("Save failed")
        || m.starts_with("Auto-saved ")
        || m.contains(" need Save As")
    {
        Some(OperationKey::Save)
    } else if m.starts_with("Opened folder")
        || m.starts_with("Open failed")
        || m.starts_with("Recent file missing")
        || m.starts_with("Recent folder missing")
    {
        Some(OperationKey::Open)
    } else if m.starts_with("Created file")
        || m.starts_with("File already exists")
        || m.starts_with("File create failed")
    {
        Some(OperationKey::CreateFile)
    } else if m.starts_with("Created folder")
        || m.starts_with("Folder already exists")
        || m.starts_with("Folder create failed")
    {
        Some(OperationKey::CreateFolder)
    } else if m.starts_with("Renamed to")
        || m.starts_with("Rename failed")
        || m.starts_with("Already named")
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
        || m.starts_with("Could not copy")
    {
        Some(OperationKey::Copy)
    } else {
        None
    }
}

/// Re-alpha a color (multiplying the existing alpha by `a`).
fn with_alpha(c: MuiColor, a: f32) -> MuiColor {
    MuiColor::new(c.r, c.g, c.b, (c.a * a).clamp(0.0, 1.0))
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
        assert!(!q.tick_at(t0 + LIFETIME - Duration::from_millis(1)));
        assert_eq!(q.len(), 1);
        // After lifetime: expired + dropped, tick reports a change.
        assert!(q.tick_at(t0 + LIFETIME + Duration::from_millis(1)));
        assert_eq!(q.len(), 0);
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
        assert!(t.presence(t0 + LIFETIME - Duration::from_millis(50)) < 0.8);
        // Past expiry: gone.
        assert_eq!(t.presence(t0 + LIFETIME + Duration::from_millis(1)), 0.0);
    }

    #[test]
    fn duplicate_message_refreshes_not_stacks() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Success, "Saved", t0);
        q.push_at(Kind::Success, "Saved", t0 + Duration::from_millis(500));
        // Still one toast, but its clock was refreshed (won't expire at t0+LIFETIME).
        assert_eq!(q.len(), 1);
        assert!(!q.tick_at(t0 + LIFETIME + Duration::from_millis(1)));
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
    fn dismiss_at_removes_clicked_toast_only() {
        let mut q = ToastQueue::new();
        let t0 = Instant::now();
        q.push_at(Kind::Info, "top", t0);
        q.push_at(Kind::Warn, "bottom", t0);
        assert!(!q.dismiss_at(900, 600, 10.0, 10.0, t0 + Duration::from_millis(500)));

        // Newest toast is bottom-most, right-aligned.
        assert!(q.dismiss_at(900, 600, 860.0, 530.0, t0 + Duration::from_millis(500)));
        assert_eq!(q.len(), 1);
        assert_eq!(q.toasts()[0].message, "top");
    }
}
