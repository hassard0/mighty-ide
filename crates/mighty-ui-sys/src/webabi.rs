//! Scalar `mui_web_*` ABI for the **Web Playground** ("Run in Browser"):
//! build the active Mighty file to `wasm32-web` and run it in the browser.
//!
//! Same shim-owns-everything, scalar-only shape as the rest of the ABI (L17):
//! the IDE opens / drives / reads back via these entry points and calls
//! [`mui_web_draw`] each frame; all state + the background `mty serve` /
//! `mty build` process live in [`crate::web`]. The Web panel renders in the
//! same lower band as the Run panel (only one is open at a time).
//!
//! The served URL is read back to Mighty char-by-char ([`mui_web_url_len`] /
//! [`mui_web_url_char`]) so the IDE can open the default browser; output lines
//! are likewise readable ([`mui_web_line_count`] / [`mui_web_line_char`]) though
//! the panel draws them itself.

use crate::layout;
use crate::theme;
use crate::web::Mode;
use crate::MuiContext;

#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

fn active_path(ctx: &MuiContext) -> Option<std::path::PathBuf> {
    ctx.tabs.active_path()
}

/// Default bind port for the Web Playground (overridable via `MIGHTY_WEB_PORT`).
fn web_port() -> u16 {
    std::env::var("MIGHTY_WEB_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(8000)
}

/// Start the Web Playground for the active file. Opens the panel + closes the
/// Run panel (they share the band). Returns `1` if a process spawned, else `0`
/// (no file / spawn or build error — the panel still shows the error output).
#[no_mangle]
pub extern "C" fn mui_web_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = active_path(ctx) else {
        ctx.web.open();
        ctx.push_toast(crate::toast::Kind::Warn, "Run in Browser: no active file");
        return 0;
    };
    // Only one bottom-band panel visible at a time.
    ctx.run.close();
    let ok = ctx.web.start(&path, web_port());
    let mode = match ctx.web.mode() {
        Mode::Serve => "mty serve",
        Mode::Build => "mty build --target wasm32-web",
        Mode::Idle => "web",
    };
    if ok {
        println!("web: started `{mode}` for {}", path.display());
        ctx.push_toast(crate::toast::Kind::Info, format!("Run in Browser: {mode}\u{2026}"));
        1
    } else {
        if ctx.web.take_saw_error() {
            ctx.push_toast(crate::toast::Kind::Error, "Run in Browser: build failed (see panel)");
        }
        0
    }
}

/// Stop the running server (best-effort kill). No-op if idle.
#[no_mangle]
pub extern "C" fn mui_web_stop(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.web.stop();
    }
}

/// Toggle the Web panel open/closed. Returns `1` if now open, `0` if closed.
#[no_mangle]
pub extern "C" fn mui_web_toggle(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.web.toggle()))
}

/// `1` if the Web panel is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_web_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.web.is_active()))
}

/// `1` while the server is still running, else `0`.
#[no_mangle]
pub extern "C" fn mui_web_running(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.web.is_running()))
}

/// Drain pending output, scrape the URL, detect completion. Returns `1` if the
/// served URL just became available this frame (so the IDE opens the browser),
/// else `0`. Fires the build-error / server-stopped toasts. Call once per frame
/// while the panel is open.
#[no_mangle]
pub extern "C" fn mui_web_pump(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let _changed = c.web.pump();
    if c.web.take_saw_error() {
        c.push_toast(crate::toast::Kind::Error, "Web build error (see panel)");
    }
    if c.web.take_just_finished() && !c.web.is_running() {
        c.push_toast(crate::toast::Kind::Info, "Web server stopped");
    }
    // Latch: a fresh URL means "open the browser now".
    i32::from(c.web.take_url_fresh())
}

/// Open the current served URL in the default browser. Returns `1` if the
/// launcher spawned (the URL is non-empty). Called by the IDE when
/// [`mui_web_pump`] returns `1`.
#[no_mangle]
pub extern "C" fn mui_web_open_browser(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let url = c.web.url().to_string();
    if crate::web::open_in_browser(&url) {
        println!("web: opened {url} in the default browser");
        c.push_toast(crate::toast::Kind::Success, format!("Opened {url}"));
        1
    } else {
        0
    }
}

/// Length (in chars) of the served URL, or `0` if none yet.
#[no_mangle]
pub extern "C" fn mui_web_url_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.web.url().chars().count() as i32)
}

/// The `i`-th char (codepoint) of the served URL, or `0` if out of range. Lets
/// Mighty read the URL back without holding a string across FFI (L21).
#[no_mangle]
pub extern "C" fn mui_web_url_char(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| {
        c.web.url().chars().nth(i as usize).map_or(0, |ch| ch as i32)
    })
}

/// Number of build/serve output lines.
#[no_mangle]
pub extern "C" fn mui_web_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.web.line_count() as i32)
}

/// Length (in chars) of output line `i`, or `0` if out of range.
#[no_mangle]
pub extern "C" fn mui_web_line_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| {
        c.web.line(i as usize).map_or(0, |l| l.text.chars().count() as i32)
    })
}

/// The `j`-th char of output line `i`, or `0` if out of range.
#[no_mangle]
pub extern "C" fn mui_web_line_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| {
        c.web
            .line(i as usize)
            .and_then(|l| l.text.chars().nth(j as usize))
            .map_or(0, |ch| ch as i32)
    })
}

/// Scroll the output by `delta` lines.
#[no_mangle]
pub extern "C" fn mui_web_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.web.scroll(delta);
    }
}

/// Map the last click to a Web-panel click code: `-1` none, `1` the "Stop
/// server" button, `2` the "Open in browser" pill (the URL). The IDE dispatches
/// the action.
pub const WEB_CLICK_NONE: i32 = -1;
pub const WEB_CLICK_STOP: i32 = 1;
pub const WEB_CLICK_OPEN: i32 = 2;

#[no_mangle]
pub extern "C" fn mui_web_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return WEB_CLICK_NONE;
    };
    if !ctx.web.is_active() {
        return WEB_CLICK_NONE;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let g = web_geom(ctx);
    // Hit-test the header action buttons (right-aligned).
    if y >= g.y0 && y <= g.y0 + g.header_h {
        if let Some(r) = g.stop_btn {
            if x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3 {
                return WEB_CLICK_STOP;
            }
        }
        if let Some(r) = g.open_btn {
            if x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3 {
                return WEB_CLICK_OPEN;
            }
        }
    }
    WEB_CLICK_NONE
}

/// Geometry of the Web panel (the same lower band as the Run panel).
struct WebGeom {
    x0: f32,
    x1: f32,
    y0: f32,
    rows_top: f32,
    panel_h: f32,
    header_h: f32,
    /// (x, y, w, h) of the Stop button, when running.
    stop_btn: Option<(f32, f32, f32, f32)>,
    /// (x, y, w, h) of the Open-in-browser pill, when a URL exists.
    open_btn: Option<(f32, f32, f32, f32)>,
}

fn web_geom(ctx: &MuiContext) -> WebGeom {
    let region = layout::region(ctx.sidebar_visible);
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height;
    let panel_h = layout::term_panel_height(h);
    let y0 = layout::term_panel_top(h);
    let header_h = layout::term_header_h();
    let x1 = w;
    // Right-aligned header buttons: [Stop] then the URL pill.
    let chrome = theme::CHROME_FONT_SIZE;
    let btn_h = 18.0;
    let by = y0 + (header_h - btn_h) * 0.5;
    let mut cursor = x1 - 12.0;
    let mut stop_btn = None;
    let mut open_btn = None;
    if ctx.web.is_running() {
        let label_w = 4.0 * (chrome * 0.55) + 22.0; // "Stop"
        cursor -= label_w;
        stop_btn = Some((cursor, by, label_w, btn_h));
        cursor -= 8.0;
    }
    let url = ctx.web.url();
    if !url.is_empty() {
        let pill_w = (url.chars().count() as f32 * (chrome * 0.5)).min(360.0) + 22.0;
        cursor -= pill_w;
        open_btn = Some((cursor, by, pill_w, btn_h));
    }
    WebGeom {
        x0: region.left,
        x1,
        y0,
        rows_top: y0 + header_h,
        panel_h,
        header_h,
        stop_btn,
        open_btn,
    }
}

/// Draw the Web Playground as a lower band: header (globe icon + "WEB" + the
/// package basename + mode + Open-URL pill + Stop), then the scrollable
/// build/serve output. No-op when closed.
#[no_mangle]
pub extern "C" fn mui_web_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.web.is_active() {
        return;
    }
    use crate::icons;
    let g = web_geom(ctx);
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let line_h = layout::LINE_H();
    let w = g.x1 - g.x0;

    // Panel surface + top divider + header band.
    ctx.dl_rect(g.x0, g.y0, w, g.panel_h, theme::BG_2());
    ctx.dl_rect(g.x0, g.y0, w, 1.0, theme::BORDER());
    ctx.dl_rect(g.x0, g.y0, w, g.header_h, theme::BG_1());
    ctx.dl_rect(g.x0, g.y0 + g.header_h - 1.0, w, 1.0, theme::BORDER_SOFT());

    let hy = g.y0 + (g.header_h - chrome) * 0.5 - 1.0;
    ctx.dl_icon(
        g.x0 + 12.0,
        g.y0 + (g.header_h - 13.0) * 0.5,
        13.0,
        13.0,
        icons::GLOBE,
        theme::INFO(),
        1.6,
        false,
    );
    ctx.text.queue_ui_sized(g.x0 + 32.0, hy, "WEB", theme::DIM(), chrome - 1.0, clip);

    // Package / file basename + the mode subtitle.
    let base = ctx
        .web
        .path()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string();
    ctx.text.queue_ui_sized(g.x0 + 66.0, hy, &base, theme::TEXT_1(), chrome - 1.0, clip);
    let mode = match ctx.web.mode() {
        Mode::Serve => "mty serve",
        Mode::Build => "wasm32-web + static",
        Mode::Idle => "",
    };
    let mode_x = g.x0 + 66.0 + (base.chars().count() as f32 + 1.0) * (chrome * 0.55) + 8.0;
    ctx.text.queue_ui_sized(mode_x, hy, mode, theme::TEXT_3(), chrome - 2.0, clip);

    // Open-in-browser pill (the URL), accent-tinted + clickable.
    if let Some((bx, by, bw, bh)) = g.open_btn {
        ctx.dl_round(bx, by, bw, bh, 6.0, theme::accent_a(0.18));
        ctx.dl_stroke(bx, by, bw, bh, 6.0, theme::ACCENT(), 1.0);
        let url = ctx.web.url().to_string();
        let avail = ((bw - 16.0) / (chrome * 0.5)).floor().max(1.0) as usize;
        let shown = if url.chars().count() > avail {
            url.chars().take(avail.saturating_sub(1)).collect::<String>() + "\u{2026}"
        } else {
            url
        };
        ctx.text.queue_ui_sized(bx + 8.0, by + 3.0, &shown, theme::TEXT_1(), chrome - 2.0, clip);
    }

    // Stop button (when running).
    if let Some((bx, by, bw, bh)) = g.stop_btn {
        ctx.dl_round(bx, by, bw, bh, 6.0, theme::BG_4());
        ctx.dl_stroke(bx, by, bw, bh, 6.0, theme::ERROR(), 1.0);
        ctx.text.queue_ui_sized(bx + 10.0, by + 3.0, "Stop", theme::ERROR(), chrome - 2.0, clip);
    }

    // Output rows.
    let first = ctx.web.first();
    let visible = ((g.panel_h - g.header_h) / line_h).floor().max(0.0) as usize;
    let adv = layout::CHAR_W();
    let count = ctx.web.line_count();
    for vis in 0..visible {
        let idx = first + vis;
        if idx >= count {
            break;
        }
        let (text, is_error) = {
            let l = ctx.web.line(idx).unwrap();
            (l.text.clone(), l.is_error)
        };
        let y = g.rows_top + vis as f32 * line_h;
        let ty = y + (line_h - chrome) * 0.5 - 1.0;
        let col = if is_error { theme::ERROR() } else { theme::TEXT_1() };
        // Tint command echoes ("$ …") + the URL line so the eye lands on them.
        if text.starts_with("$ ") {
            ctx.text.queue(g.x0 + 12.0, ty, &clip_row(&text, g.x0, g.x1, adv), theme::ACCENT(), clip);
            continue;
        }
        ctx.text.queue(g.x0 + 12.0, ty, &clip_row(&text, g.x0, g.x1, adv), col, clip);
    }
}

/// Clip `text` to the panel width (ellipsizing).
fn clip_row(text: &str, x0: f32, x1: f32, adv: f32) -> String {
    let avail = (((x1 - 14.0) - (x0 + 12.0)) / adv).floor() as usize;
    if text.chars().count() > avail && avail > 1 {
        text.chars().take(avail - 1).collect::<String>() + "\u{2026}"
    } else {
        text.to_string()
    }
}
