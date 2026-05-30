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
        let message = message.into();
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
        let clip = ctx.clip;

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
                clip,
            );
            let msg = truncate(&t.message, card_w - 64.0);
            ctx.text.queue_ui_sized(
                tx,
                cy + 28.0,
                &msg,
                with_alpha(theme::TEXT(), alpha),
                13.0,
                clip,
            );
        }
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

/// Truncate `s` to roughly `max_px` wide at the UI font, appending an ellipsis.
/// Uses a coarse per-char advance estimate (the proportional UI font); good
/// enough for a one-line toast message (the renderer clips anyway).
fn truncate(s: &str, max_px: f32) -> String {
    let approx_char = 7.0_f32;
    let max_chars = (max_px / approx_char).floor().max(4.0) as usize;
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('\u{2026}');
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
}
