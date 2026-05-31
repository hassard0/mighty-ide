//! Headless offscreen tests: render-to-texture + pixel readback, plus a pure
//! event-queue FIFO test. GPU tests skip (without failing) when no adapter is
//! available — print a notice and return.

use crate::ffi::*;
use crate::window::{translate_window_event, EventQueue};
use crate::{
    mui_begin_frame, mui_draw_text, mui_end_frame, mui_fill_rect, mui_poll_event, mui_set_clip,
    mui_text_measure, MuiContext,
};

const W: u32 = 64;
const H: u32 = 64;

/// Index into RGBA8 pixel data at (x, y).
fn px(pixels: &[u8], x: u32, y: u32, width: u32) -> (u8, u8, u8, u8) {
    let i = ((y * width + x) * 4) as usize;
    (pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3])
}

fn is_clearish(c: (u8, u8, u8, u8)) -> bool {
    // CLEAR_COLOR is (0.08,0.08,0.10) -> roughly (20,20,26).
    c.0 < 60 && c.1 < 60 && c.2 < 70
}

macro_rules! ctx_or_skip {
    () => {
        match MuiContext::new_offscreen(W, H) {
            Some(c) => c,
            None => {
                eprintln!("SKIP: no GPU adapter available; skipping offscreen GPU test");
                return;
            }
        }
    };
}

#[test]
fn fill_rect_produces_red_texels_and_clear_elsewhere() {
    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;
    unsafe {
        mui_begin_frame(p);
        // Red rect at (10,10) size 5x5.
        mui_fill_rect(p, 10.0, 10.0, 5.0, 5.0, MuiColor::new(1.0, 0.0, 0.0, 1.0));
        mui_end_frame(p);
    }
    let pixels = ctx.read_pixels();

    // Center of the rect should be red.
    let inside = px(&pixels, 12, 12, W);
    assert!(
        inside.0 > 200 && inside.1 < 60 && inside.2 < 60,
        "expected red at (12,12), got {inside:?}"
    );

    // A far corner should be the clear color.
    let corner = px(&pixels, 60, 60, W);
    assert!(
        is_clearish(corner),
        "expected clear color at (60,60), got {corner:?}"
    );
}

#[test]
fn vello_rounded_rect_fills_center_and_softens_corner() {
    // The default render path is the Vello UI; a rounded rect should fill solid
    // at its center and be anti-aliased (corner pixel not fully saturated).
    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;
    unsafe {
        mui_begin_frame(p);
        // Push a rounded rect directly via the display-list helper.
        if let Some(c) = p.as_mut() {
            c.dl_round(8.0, 8.0, 40.0, 40.0, 10.0, MuiColor::new(0.0, 1.0, 0.0, 1.0));
        }
        mui_end_frame(p);
    }
    let pixels = ctx.read_pixels();
    // Center is solid green.
    let center = px(&pixels, 28, 28, W);
    assert!(
        center.1 > 200 && center.0 < 60,
        "expected solid green at center, got {center:?}"
    );
    // The extreme top-left corner of the bounding box is outside the rounded
    // corner → should be (near) clear, proving the corner was rounded.
    let corner = px(&pixels, 8, 8, W);
    assert!(
        is_clearish(corner),
        "expected rounded (clear) corner at (8,8), got {corner:?}"
    );
}

#[test]
fn text_measure_returns_positive_extents() {
    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;
    let s = b"hello";
    let (mut w, mut h) = (0.0f32, 0.0f32);
    let ok = unsafe { mui_text_measure(p, s.as_ptr(), s.len(), &mut w, &mut h) };
    assert!(ok, "measure should succeed");
    assert!(w > 0.0, "width should be > 0, got {w}");
    assert!(h > 0.0, "height should be > 0, got {h}");
}

#[test]
fn rendering_a_glyph_yields_non_clear_texels_in_its_box() {
    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;
    let s = b"M";
    // Measure to know the glyph box.
    let (mut tw, mut th) = (0.0f32, 0.0f32);
    unsafe { mui_text_measure(p, s.as_ptr(), s.len(), &mut tw, &mut th) };

    unsafe {
        mui_begin_frame(p);
        mui_draw_text(p, 2.0, 2.0, s.as_ptr(), s.len(), MuiColor::new(1.0, 1.0, 1.0, 1.0));
        mui_end_frame(p);
    }
    let pixels = ctx.read_pixels();

    // Scan the glyph's bounding box for any non-clear (drawn) texel.
    let bx = (tw.ceil() as u32 + 4).min(W);
    let by = (th.ceil() as u32 + 4).min(H);
    let mut found = false;
    for y in 0..by {
        for x in 0..bx {
            if !is_clearish(px(&pixels, x, y, W)) {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(found, "expected at least one drawn glyph texel in box {bx}x{by}");
}

#[test]
fn set_clip_clips_a_rect_outside_the_scissor() {
    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;
    unsafe {
        mui_begin_frame(p);
        // Clip to a tiny top-left box, then draw a rect entirely outside it.
        mui_set_clip(p, 0, 0, 4, 4);
        mui_fill_rect(p, 20.0, 20.0, 10.0, 10.0, MuiColor::new(1.0, 0.0, 0.0, 1.0));
        mui_end_frame(p);
    }
    let pixels = ctx.read_pixels();

    // The rect's would-be pixels must be clear (fully clipped).
    let inside_rect = px(&pixels, 25, 25, W);
    assert!(
        is_clearish(inside_rect),
        "expected clipped (clear) at (25,25), got {inside_rect:?}"
    );
}

#[test]
fn set_clip_keeps_a_rect_inside_the_scissor() {
    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;
    unsafe {
        mui_begin_frame(p);
        mui_set_clip(p, 0, 0, 32, 32);
        mui_fill_rect(p, 5.0, 5.0, 10.0, 10.0, MuiColor::new(0.0, 1.0, 0.0, 1.0));
        mui_end_frame(p);
    }
    let pixels = ctx.read_pixels();
    let inside = px(&pixels, 8, 8, W);
    assert!(
        inside.1 > 200 && inside.0 < 60,
        "expected green at (8,8) within clip, got {inside:?}"
    );
}

// ---- event queue (pure, no GPU) ----

#[test]
fn event_queue_returns_pushed_events_fifo_then_empty() {
    let mut ctx = match MuiContext::new_offscreen(W, H) {
        Some(c) => c,
        None => {
            // Even without GPU we can exercise the queue directly.
            let mut q = EventQueue::default();
            q.push(MuiEvent::char(b'a' as u32, 0));
            q.push(MuiEvent::key(MUI_KEY_ENTER, MUI_MOD_CTRL));
            assert_eq!(q.pop().unwrap().tag, MUI_EVENT_CHAR);
            assert_eq!(q.pop().unwrap().tag, MUI_EVENT_KEY);
            assert!(q.pop().is_none());
            return;
        }
    };

    ctx.queue.push(MuiEvent::char(b'a' as u32, 0));
    ctx.queue
        .push(MuiEvent::mouse(MUI_EVENT_MOUSE_DOWN, MUI_MOUSE_LEFT, 3.0, 4.0, 0));
    ctx.queue.push(MuiEvent::key(MUI_KEY_ENTER, MUI_MOD_CTRL));

    let p: *mut MuiContext = &mut ctx;
    let mut ev = MuiEvent::none();

    unsafe {
        assert!(mui_poll_event(p, &mut ev));
        assert_eq!(ev.tag, MUI_EVENT_CHAR);
        assert_eq!(ev.codepoint, b'a' as u32);

        assert!(mui_poll_event(p, &mut ev));
        assert_eq!(ev.tag, MUI_EVENT_MOUSE_DOWN);
        assert_eq!(ev.button, MUI_MOUSE_LEFT);
        assert_eq!(ev.x, 3.0);
        assert_eq!(ev.y, 4.0);

        assert!(mui_poll_event(p, &mut ev));
        assert_eq!(ev.tag, MUI_EVENT_KEY);
        assert_eq!(ev.key, MUI_KEY_ENTER);
        assert_eq!(ev.mods & MUI_MOD_CTRL, MUI_MOD_CTRL);

        // Headless context has no winit host, so no new events appear.
        assert!(!mui_poll_event(p, &mut ev));
    }
}

// ---- scalar file-I/O ABI (save staging -> write -> load -> read by index) ----

#[test]
fn save_staging_writes_then_load_reads_back_round_trip() {
    use crate::{
        mui_load, mui_load_byte, mui_path_commit, mui_path_push, mui_save_commit, mui_save_push,
    };

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    // Point the shim at a temp file by staging the path byte-by-byte.
    let dir = std::env::temp_dir();
    let path = dir.join("mui_save_roundtrip.txt");
    let _ = std::fs::remove_file(&path);
    for b in path.to_string_lossy().as_bytes() {
        mui_path_push(handle, *b as u32);
    }
    mui_path_commit(handle);

    // Stage "Hi\n!" and commit.
    for b in b"Hi\n!" {
        mui_save_push(handle, *b as u32);
    }
    assert_eq!(mui_save_commit(handle), 0, "save_commit should succeed");
    assert_eq!(std::fs::read(&path).unwrap(), b"Hi\n!");

    // Load it back and read each byte by index.
    assert_eq!(mui_load(handle), 4, "load should report 4 bytes");
    let got: Vec<i32> = (0..5).map(|i| mui_load_byte(handle, i)).collect();
    assert_eq!(got, vec![b'H' as i32, b'i' as i32, 10, b'!' as i32, -1]);

    let _ = std::fs::remove_file(&path);
}

// ---- multi-file workspace ABI (tabs + tree + click routing) ----

#[test]
fn tab_abi_open_switch_close_and_byte_round_trip() {
    use crate::langdetect::Language;
    use crate::{
        mui_dirty_confirm_active, mui_dirty_confirm_cancel, mui_dirty_confirm_discard,
        mui_dirty_confirm_save,
        mui_ed_set_dirty, mui_path_clear, mui_path_push, mui_quit_request, mui_tab_active,
        mui_tab_close, mui_tab_count, mui_tab_cursor_col, mui_tab_cursor_line, mui_tab_load,
        mui_tab_load_byte, mui_tab_open_path, mui_tab_scroll, mui_tab_set_dirty,
        mui_tab_store_begin, mui_tab_store_byte, mui_tab_store_commit, mui_tab_switch,
    };

    let mut ctx = ctx_or_skip!();
    // The offscreen context starts with an empty store; seed a scratch tab as
    // the real init path (build_context) does.
    ctx.tabs.ensure_scratch();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    // No file opened -> one scratch tab.
    assert_eq!(mui_tab_count(handle), 1);
    assert_eq!(mui_tab_active(handle), 0);

    // Open a real file as a new tab via the staged-path ABI.
    let dir = std::env::temp_dir();
    let path = dir.join("mui_tababi_open.txt");
    std::fs::write(&path, b"hello\nworld").unwrap();
    for b in path.to_string_lossy().as_bytes() {
        mui_path_push(handle, *b as u32);
    }
    let idx = mui_tab_open_path(handle);
    assert_eq!(idx, 1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);

    // Open File should not silently create a file-backed empty tab for a typo.
    mui_path_clear(handle);
    let missing = dir.join("mui_tababi_missing.txt");
    let _ = std::fs::remove_file(&missing);
    for b in missing.to_string_lossy().as_bytes() {
        mui_path_push(handle, *b as u32);
    }
    assert_eq!(mui_tab_open_path(handle), -1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);
    mui_path_clear(handle);

    // The confirmation overlay can save a dirty file-backed tab before closing.
    let save_path = dir.join("mui_tababi_save_confirm.txt");
    std::fs::write(&save_path, b"save me").unwrap();
    for b in save_path.to_string_lossy().as_bytes() {
        mui_path_push(handle, *b as u32);
    }
    assert_eq!(mui_tab_open_path(handle), 2);
    mui_tab_set_dirty(handle, 2, 1);
    assert_eq!(mui_tab_close(handle, 2), -1);
    assert_eq!(mui_dirty_confirm_save(handle), 1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);
    mui_path_clear(handle);

    // Dirty tabs require a second close request before discarding edits.
    mui_tab_set_dirty(handle, 1, 1);
    assert_eq!(mui_quit_request(handle), 0);
    assert_eq!(mui_dirty_confirm_active(handle), 1);
    mui_dirty_confirm_cancel(handle);
    assert_eq!(mui_dirty_confirm_active(handle), 0);
    assert_eq!(mui_quit_request(handle), 0);
    assert_eq!(mui_quit_request(handle), 1);
    assert_eq!(mui_tab_close(handle, 1), -1);
    assert_eq!(mui_dirty_confirm_active(handle), 1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);
    mui_dirty_confirm_cancel(handle);
    mui_tab_set_dirty(handle, 1, 0);
    assert_eq!(mui_quit_request(handle), 1);
    mui_ed_set_dirty(handle, 1);
    assert_eq!(mui_quit_request(handle), 0);
    assert_eq!(mui_dirty_confirm_discard(handle), -2);
    mui_ed_set_dirty(handle, 0);
    assert_eq!(mui_quit_request(handle), 1);

    // Its bytes are readable via the tab-load ABI.
    assert_eq!(mui_tab_load(handle, 1), 11);
    let got: Vec<i32> = (0..3).map(|i| mui_tab_load_byte(handle, 1, i)).collect();
    assert_eq!(got, vec![b'h' as i32, b'e' as i32, b'l' as i32]);

    // Byte-swap: store a fresh buffer + state into tab 0, read it back.
    mui_tab_store_begin(handle, 0);
    for b in b"AB\nC" {
        mui_tab_store_byte(handle, 0, *b as i32);
    }
    mui_tab_store_commit(handle, 0, 1, 0, 0);
    mui_tab_switch(handle, 0);
    assert_eq!(mui_tab_active(handle), 0);
    assert_eq!(ctx.language, Language::Mighty);
    assert_eq!(mui_tab_load(handle, 0), 4);
    assert_eq!(mui_tab_cursor_line(handle, 0), 1);
    assert_eq!(mui_tab_cursor_col(handle, 0), 0);
    assert_eq!(mui_tab_scroll(handle, 0), 0);

    // First close on a dirty tab warns; second close on the same tab discards.
    mui_tab_set_dirty(handle, 0, 1);
    assert_eq!(mui_tab_close(handle, 0), -1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_dirty_confirm_discard(handle), 0);
    // Close tab 0 -> tab 1 remains, count 1.
    assert_eq!(mui_tab_count(handle), 1);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&save_path);
}

#[test]
fn tree_abi_scan_toggle_and_open_row() {
    use crate::{
        mui_tab_count, mui_tree_count, mui_tree_is_dir, mui_tree_open_row, mui_tree_refresh,
        mui_tree_toggle,
    };

    let mut ctx = ctx_or_skip!();
    // Point the tree at a temp dir with a known shape.
    let root = std::env::temp_dir().join("mui_treeabi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("deep.txt"), b"deep").unwrap();
    std::fs::write(root.join("a.txt"), b"hi").unwrap();
    ctx.tree.set_root(root.clone());

    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    assert_eq!(mui_tree_refresh(handle), 2); // sub/ + a.txt
    assert_eq!(mui_tree_count(handle), 2);
    assert_eq!(mui_tree_is_dir(handle, 0), 1); // sub/
    assert_eq!(mui_tree_is_dir(handle, 1), 0); // a.txt

    // Expand the dir -> deep.txt splices in.
    assert_eq!(mui_tree_toggle(handle, 0), 3);

    // Opening the file row (a.txt is now at row 2 after expand) opens a tab.
    let before = mui_tab_count(handle);
    let opened = mui_tree_open_row(handle, 2);
    assert!(opened >= 0, "expected a file row to open, got {opened}");
    assert_eq!(mui_tab_count(handle), before + 1);

    // Opening a directory row is a no-op (returns -1).
    assert_eq!(mui_tree_open_row(handle, 0), -1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn click_routing_tab_bar_sidebar_and_text() {
    use crate::ffi::MuiEvent;
    use crate::{
        mui_rail_utility_at_click, mui_tab_close_index_at_click, mui_tab_index_at_click,
        mui_tree_row_at_click,
    };
    use crate::layout;
    use crate::panels::mui_ai_click;

    let mut ctx = ctx_or_skip!();
    // Two tabs so index 1 is valid.
    ctx.tabs.ensure_scratch();
    ctx.tabs
        .open_path(std::env::temp_dir().join("mui_click_b.txt"));
    // A tree with a couple rows.
    let root = std::env::temp_dir().join("mui_clickrt");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("x.txt"), b"x").unwrap();
    ctx.tree.set_root(root.clone());
    ctx.sidebar_visible = true;
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;

    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    // Click in the tab bar over tab 1. Tabs start right of the rail AND the
    // sidebar (when shown), matching `mui_tab_bar_draw`.
    let body_left = layout::RAIL_W + layout::SIDEBAR_W;
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, body_left + layout::TAB_W + 5.0, 4.0, 0);
    assert_eq!(mui_tab_index_at_click(handle), 1);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        body_left + layout::TAB_W + layout::TAB_W - 20.0,
        4.0,
        0,
    );
    assert_eq!(mui_tab_close_index_at_click(handle), 1);
    // The top-right run/menu/window-control strip is not a tab, even though it
    // shares the tab-bar row.
    let reserved_x = crate::titlebar::controls_x(ctx.gpu.width as f32)
        - crate::titlebar::ACTION_STRIP_W
        + 4.0;
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, reserved_x, 4.0, 0);
    assert_eq!(mui_tab_index_at_click(handle), -1);
    assert_eq!(mui_tab_close_index_at_click(handle), -1);
    // Same x but below the tab bar -> not a tab click.
    ctx.last_event.y = layout::TAB_BAR_H + 50.0;
    assert_eq!(mui_tab_index_at_click(handle), -1);
    assert_eq!(mui_tab_close_index_at_click(handle), -1);

    // Click in the sidebar over row 0 (sidebar content is right of the rail).
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        layout::RAIL_W + 10.0,
        layout::TAB_BAR_H + 2.0,
        0,
    );
    assert_eq!(mui_tree_row_at_click(handle), 0);
    // Click right of the sidebar (in text area) -> not a tree click.
    ctx.last_event.x = layout::RAIL_W + layout::SIDEBAR_W + 100.0;
    assert_eq!(mui_tree_row_at_click(handle), -1);
    // Click in the activity rail (left of the sidebar) -> not a tree click.
    ctx.last_event.x = 10.0;
    assert_eq!(mui_tree_row_at_click(handle), -1);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        20.0,
        ctx.gpu.height as f32 - 32.0,
        0,
    );
    assert_eq!(mui_rail_utility_at_click(handle), 2);

    // The right-docked AI panel owns its surface, including the send affordance,
    // while still leaving the top-right chrome strip to title-bar actions.
    ctx.ai.open = true;
    ctx.ai.input = "ship it".to_string();
    let (px, pw, input_y, input_h) =
        crate::ai::input_geometry(&ctx.ai.input, ctx.gpu.width, ctx.gpu.height);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        px + pw - 24.0,
        input_y + input_h - 20.0,
        0,
    );
    assert_eq!(mui_ai_click(handle), 2);
    ctx.last_event.x = px + 24.0;
    assert_eq!(mui_ai_click(handle), 1);
    ctx.last_event.x = px - 2.0;
    assert_eq!(mui_ai_click(handle), 0);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, reserved_x, 4.0, 0);
    assert_eq!(mui_ai_click(handle), 0);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn search_panel_clicks_focus_fields_and_return_actions() {
    use crate::ffi::MuiEvent;

    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = true;
    ctx.active_panel = crate::PANEL_SEARCH;
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::SIDEBAR_W;

    ctx.search.replace_focus = true;
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + 24.0, 52.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 0);
    assert!(!ctx.search.replace_focus);

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + 24.0, 88.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 0);
    assert!(ctx.search.replace_focus);

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + sw - 26.0, 52.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 1);
    assert!(!ctx.search.replace_focus);

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + sw - 26.0, 88.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 2);
    assert!(ctx.search.replace_focus);

    ctx.active_panel = crate::PANEL_EXPLORER;
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 0);
}

#[test]
fn search_replace_all_toasts_visible_result() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_search_replace_toast");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.mty"), "foo\nfoo\n").unwrap();
    ctx.tree.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    for ch in "foo".chars() {
        ctx.search.push_char(ch as u32);
    }
    assert_eq!(crate::panels::mui_search_run(handle), 2);
    ctx.search.replace_focus = true;
    for ch in "bar".chars() {
        ctx.search.push_char(ch as u32);
    }

    assert_eq!(crate::panels::mui_search_replace_all(handle), 2);
    assert_eq!(std::fs::read_to_string(root.join("a.mty")).unwrap(), "bar\nbar\n");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Replaced 2 occurrences");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn topbar_actions_hit_run_and_menu_but_not_in_zen() {
    use crate::ffi::MuiEvent;
    use crate::mui_topbar_action_at_click;

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::layout::zen_active();
    crate::layout::set_zen(false);

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let controls_x = crate::titlebar::controls_x(ctx.gpu.width as f32);
    let run_x = controls_x - 60.0 + 8.0;
    let menu_x = controls_x - 60.0 + 32.0;

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, run_x, 4.0, 0);
    assert_eq!(mui_topbar_action_at_click(handle), 1);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, menu_x, 4.0, 0);
    assert_eq!(mui_topbar_action_at_click(handle), 2);
    ctx.last_event.y = crate::layout::TAB_BAR_H + 1.0;
    assert_eq!(mui_topbar_action_at_click(handle), 0);

    crate::layout::set_zen(true);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, run_x, 4.0, 0);
    assert_eq!(mui_topbar_action_at_click(handle), 0);

    crate::layout::set_zen(before);
}

#[test]
fn status_problems_chip_hit_tracks_rendered_branch_width() {
    use crate::ffi::MuiEvent;
    use crate::{mui_status_problems_chip_at_click, mui_status_render};

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    ctx.scm.status.branch = "feature/very-long-branch-name".to_string();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    mui_status_render(handle, 2);
    let (x, y, w, h) = ctx
        .status_problems_rect
        .expect("status render should record the Problems chip rect");
    assert!(x > 210.0, "long branch should push chip beyond the old fixed hit range");

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        x + w * 0.5,
        y + h * 0.5,
        0,
    );
    assert_eq!(mui_status_problems_chip_at_click(handle), 1);
    ctx.last_event.x = x - 8.0;
    assert_eq!(mui_status_problems_chip_at_click(handle), 0);
}

#[test]
fn chord_command_id_resolves_palette_commands_for_mighty_dispatch() {
    use crate::mui_chord_command_id;
    use crate::shortcuts::{Chord, MOD_ALT, MOD_CTRL, MOD_SHIFT};

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    assert_eq!(
        mui_chord_command_id(handle, 'n' as i32, MOD_CTRL),
        crate::palette::CMD_NEW_FILE as i32
    );
    assert_eq!(
        mui_chord_command_id(handle, 'n' as i32, MOD_CTRL | MOD_SHIFT),
        crate::palette::CMD_NEW_FOLDER as i32
    );
    assert_eq!(
        mui_chord_command_id(handle, 's' as i32, MOD_CTRL),
        crate::palette::CMD_SAVE as i32
    );
    assert_eq!(
        mui_chord_command_id(handle, 's' as i32, MOD_CTRL | MOD_SHIFT),
        crate::palette::CMD_SAVE_AS as i32
    );
    ctx.shortcuts
        .overrides_mut()
        .set(crate::palette::CMD_SAVE, Chord::new('k' as i32, MOD_ALT));
    assert_eq!(
        mui_chord_command_id(handle, 'k' as i32, MOD_ALT),
        crate::palette::CMD_SAVE as i32
    );
    assert_eq!(
        mui_chord_command_id(handle, 's' as i32, MOD_CTRL),
        -2,
        "old default should be consumed after remap"
    );
}

// ---- offscreen screenshot mode (PNG written, non-empty, correct dims) ----

#[test]
fn screenshot_renders_a_frame_and_writes_a_nonempty_png() {
    use crate::screenshot;

    let mut ctx = ctx_or_skip!();
    let p: *mut MuiContext = &mut ctx;

    // Draw a representative frame: a clear background plus a colored rect and a
    // glyph, mirroring what the live editor issues each frame.
    unsafe {
        mui_begin_frame(p);
        mui_fill_rect(p, 4.0, 4.0, 20.0, 12.0, MuiColor::new(0.2, 0.5, 0.9, 1.0));
        mui_draw_text(p, 6.0, 6.0, b"Mi".as_ptr(), 2, MuiColor::new(1.0, 1.0, 1.0, 1.0));
        mui_end_frame(p);
    }

    let pixels = ctx.read_pixels();
    assert_eq!(
        pixels.len(),
        (W * H * 4) as usize,
        "expected tightly-packed RGBA8 of {W}x{H}"
    );

    let path = std::env::temp_dir().join("mui_screenshot_test.png");
    let _ = std::fs::remove_file(&path);
    let bytes = screenshot::write_png(&path, W, H, &pixels).expect("write_png");
    assert!(bytes > 0, "PNG should be non-empty, got {bytes} bytes");

    // It must be a real PNG (magic) and decode back to the requested dimensions.
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[..8], b"\x89PNG\r\n\x1a\n", "PNG magic header");
    let decoder = png::Decoder::new(std::io::Cursor::new(&raw));
    let reader = decoder.read_info().expect("png decode");
    let info = reader.info();
    assert_eq!((info.width, info.height), (W, H), "decoded PNG dimensions");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn editor_abi_drives_live_model_and_undo() {
    use crate::{
        mui_ed_backspace, mui_ed_cursor_col, mui_ed_cursor_line, mui_ed_insert_char,
        mui_ed_line_count, mui_ed_move, mui_ed_newline, mui_ed_redo, mui_ed_undo,
        mui_ed_undo_record,
    };
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    // Type "hi", newline, "x". The model must reflect each edit LIVE.
    mui_ed_insert_char(h, 'h' as i32);
    mui_ed_insert_char(h, 'i' as i32);
    assert_eq!(mui_ed_line_count(h), 1);
    assert_eq!(mui_ed_cursor_col(h), 2);

    mui_ed_newline(h);
    mui_ed_insert_char(h, 'x' as i32);
    assert_eq!(mui_ed_line_count(h), 2);
    assert_eq!(mui_ed_cursor_line(h), 1);
    assert_eq!(mui_ed_cursor_col(h), 1);

    mui_ed_backspace(h);
    assert_eq!(mui_ed_cursor_col(h), 0);

    // Movement clamps within bounds.
    mui_ed_move(h, crate::editor::DIR_LEFT); // wraps to end of line 0
    assert_eq!(mui_ed_cursor_line(h), 0);
    assert_eq!(mui_ed_cursor_col(h), 2);

    // Undo/redo round-trip: checkpoint, edit, undo restores, redo re-applies.
    mui_ed_undo_record(h);
    mui_ed_move(h, crate::editor::DIR_END);
    mui_ed_insert_char(h, '!' as i32);
    let after = mui_ed_cursor_col(h);
    assert_eq!(mui_ed_undo(h), 1);
    // After undo the '!' edit is gone (line 0 back to "hi").
    assert!(mui_ed_cursor_col(h) <= after);
    assert_eq!(mui_ed_redo(h), 1);
}

#[test]
fn pane_split_focus_close_via_abi() {
    use crate::ffi::MuiEvent;
    use crate::{
        mui_pane_close, mui_pane_count, mui_pane_dispatch, mui_pane_focus_at_click,
        mui_pane_focus_next, mui_pane_focused, mui_pane_split_right, mui_pane_tab,
        mui_tab_active,
    };

    use crate::editor::TextModel;
    let mut ctx = ctx_or_skip!();
    // Seed two real tabs (scratch + one opened file) so a pane can show each.
    // Give each model 40 lines so scroll offsets (7, 20) don't clamp.
    let many = b"l\n".repeat(40);
    ctx.tabs.ensure_scratch();
    ctx.tabs
        .open_path(std::env::temp_dir().join("mui_pane_b.txt"));
    ctx.tabs.switch(1);
    *ctx.tabs.active_model_mut() = TextModel::from_bytes(&many);
    // Make tab 0 the active/left tab, bind the single pane to it (the unsplit
    // invariant), and give it a distinct scroll so we can prove per-pane restore.
    ctx.tabs.switch(0);
    *ctx.tabs.active_model_mut() = TextModel::from_bytes(&many);
    ctx.tabs.active_model_mut().set_first_visible(7);
    ctx.panes = crate::panes::PaneLayout::new(0);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    // --- INVARIANT: one pane behaves exactly as before ---------------------
    assert_eq!(mui_pane_count(h), 1);
    assert_eq!(mui_pane_focused(h), 0);
    assert_eq!(mui_pane_tab(h, 0), 0);
    // Focus-next / close are no-ops with one pane (active tab unchanged).
    assert_eq!(mui_pane_focus_next(h), 0);
    assert_eq!(mui_pane_close(h), 1);
    assert_eq!(mui_tab_active(h), 0);

    // --- split -> two panes, new (right) pane focused, active tab rebinds --
    assert_eq!(mui_pane_split_right(h), 2);
    assert_eq!(mui_pane_count(h), 2);
    assert_eq!(mui_pane_focused(h), 1);
    // split_right clones the focused pane's tab, so both show tab 0 here.
    assert_eq!(mui_pane_tab(h, 0), 0);
    assert_eq!(mui_pane_tab(h, 1), 0);
    // The focused pane's tab IS the active tab.
    assert_eq!(mui_tab_active(h), 0);

    // Point the right (focused) pane at the other tab via the tab-switch path,
    // then scroll it; this is the per-pane scroll we must restore later.
    {
        let ctx = unsafe { &mut *(h as usize as *mut MuiContext) };
        ctx.tabs.switch(1);
        ctx.panes.set_tab(1, 1);
        ctx.tabs.active_model_mut().set_first_visible(20);
    }
    assert_eq!(mui_pane_tab(h, 1), 1);

    // --- focus pane 0: active tab rebinds to tab 0 + restores its scroll ----
    let f0 = mui_pane_focus_next(h); // wraps 1 -> 0
    assert_eq!(f0, 0);
    assert_eq!(mui_tab_active(h), 0);
    {
        let ctx = unsafe { &mut *(h as usize as *mut MuiContext) };
        assert_eq!(ctx.tabs.active_model().first_visible(), 7, "left pane scroll restored");
    }

    // --- click in the RIGHT pane's column focuses pane 1 + restores scroll --
    {
        let ctx = unsafe { &mut *(h as usize as *mut MuiContext) };
        let region = crate::layout::region(ctx.sidebar_visible);
        let win_w = ctx.gpu.width as f32;
        let (l1, _r1) = crate::layout::pane_bounds(region, win_w, 2, 1);
        // A click just inside the right column.
        ctx.last_event =
            MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, l1 + 1.0, region.top + 5.0, 0);
    }
    assert_eq!(mui_pane_focus_at_click(h), 1);
    assert_eq!(mui_pane_focused(h), 1);
    assert_eq!(mui_tab_active(h), 1);
    {
        let ctx = unsafe { &mut *(h as usize as *mut MuiContext) };
        assert_eq!(ctx.tabs.active_model().first_visible(), 20, "right pane scroll restored");
    }

    // --- close the focused pane -> back to the single-pane state -----------
    assert_eq!(mui_pane_close(h), 1);
    assert_eq!(mui_pane_count(h), 1);
    assert_eq!(mui_pane_focused(h), 0);
    // The surviving (left) pane shows tab 0 and is the active tab.
    assert_eq!(mui_pane_tab(h, 0), 0);
    assert_eq!(mui_tab_active(h), 0);

    // --- palette dispatch routes the same as the direct ops ----------------
    assert_eq!(mui_pane_dispatch(h, crate::palette::CMD_SPLIT_RIGHT as i32), 2);
    assert_eq!(mui_pane_dispatch(h, crate::palette::CMD_CLOSE_PANE as i32), 1);
    // An out-of-block id is ignored (returns the current count, no panic).
    assert_eq!(mui_pane_dispatch(h, 0), 1);

    let _ = std::fs::remove_file(std::env::temp_dir().join("mui_pane_b.txt"));
}

#[test]
fn editor_power_features_via_abi() {
    use crate::{
        mui_ed_backspace_smart, mui_ed_bracket_match, mui_ed_duplicate, mui_ed_insert_char,
        mui_ed_insert_smart, mui_ed_line_count, mui_ed_move_lines_down, mui_ed_move_to,
        mui_ed_newline_indent, mui_ed_toggle_comment, mui_replace_active, mui_replace_all,
        mui_replace_open, mui_replace_push, mui_replace_toggle_focus,
    };
    // Auto-indent reads the global tab width; pin defaults under the shared
    // settings test lock so a parallel settings test can't leave it at 4 (the
    // brace-indent assertion below expects a 2-space indent). Build the context
    // FIRST — `build_context` calls `settings::load_into_active()`, which can pull
    // a persisted tab_width a parallel settings test wrote — then pin defaults so
    // our assertion is deterministic.
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    crate::settings::set_active(crate::settings::Settings::default());
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    // Toggle comment on a freshly-typed line.
    for c in "let x = 1".chars() {
        mui_ed_insert_char(h, c as i32);
    }
    mui_ed_toggle_comment(h);
    assert_eq!(ctx.tabs.active_model().line(0), "// let x = 1");
    mui_ed_toggle_comment(h);
    assert_eq!(ctx.tabs.active_model().line(0), "let x = 1");

    // Auto-close: typing '(' inserts a pair and reports smart-handled.
    let h = (&mut ctx as *mut MuiContext) as usize as i64;
    mui_ed_move_to(h, 0, 9);
    assert_eq!(mui_ed_insert_smart(h, '(' as i32), 1);
    assert_eq!(ctx.tabs.active_model().line(0), "let x = 1()");
    // Pair-backspace removes both.
    assert_eq!(mui_ed_backspace_smart(h), 1);
    assert_eq!(ctx.tabs.active_model().line(0), "let x = 1");

    // Auto-indent on Enter after a brace.
    let h = (&mut ctx as *mut MuiContext) as usize as i64;
    for c in " {".chars() {
        mui_ed_insert_char(h, c as i32);
    }
    mui_ed_newline_indent(h);
    assert_eq!(ctx.tabs.active_model().line(1), "  ");

    // Duplicate + move line down.
    let h = (&mut ctx as *mut MuiContext) as usize as i64;
    let before = mui_ed_line_count(h);
    mui_ed_duplicate(h);
    assert_eq!(mui_ed_line_count(h), before + 1);
    mui_ed_move_lines_down(h);

    // Bracket match: place cursor before a '(' typed earlier — none here, so 0.
    let _ = mui_ed_bracket_match(h);

    // In-file replace bar: open seeds the find field from the word under the
    // cursor ("foo"); type the replacement; replace-all.
    let h = (&mut ctx as *mut MuiContext) as usize as i64;
    *ctx.tabs.active_model_mut() = crate::editor::TextModel::from_bytes(b"foo foo foo");
    ctx.tabs.active_model_mut().move_to(0, 0);
    mui_replace_open(h); // seeds find = "foo"
    assert_eq!(mui_replace_active(h), 1);
    assert_eq!(mui_replace_toggle_focus(h), 1); // focus replace field
    for c in "bar".chars() {
        mui_replace_push(h, c as i32);
    }
    assert_eq!(mui_replace_all(h), 3);
    assert_eq!(ctx.tabs.active_model().line(0), "bar bar bar");
}

#[test]
fn welcome_active_when_no_file_open_then_inactive_after_edit() {
    use crate::{
        mui_ed_insert_char, mui_tab_new_untitled, mui_welcome_active, mui_welcome_dismiss,
        mui_welcome_open,
    };
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    // Fresh offscreen context: a scratch tab with no path + empty buffer → the
    // Welcome screen is active.
    assert_eq!(mui_welcome_active(h), 1);

    // Typing into the (still path-less) buffer makes it non-empty → Welcome off.
    mui_ed_insert_char(h, 'x' as i32);
    assert_eq!(mui_welcome_active(h), 0);

    // The palette "Welcome" command can force it back open regardless of buffer.
    mui_welcome_open(h);
    assert_eq!(mui_welcome_active(h), 1);
    mui_welcome_dismiss(h);
    assert_eq!(mui_welcome_active(h), 0);

    // Explicit New File is not the same as startup/no-file. It should reveal a
    // blank editor immediately instead of letting the automatic Welcome state
    // reclaim the body.
    let ni = mui_tab_new_untitled(h);
    assert!(ni >= 0);
    assert_eq!(mui_welcome_active(h), 0);
}

#[test]
fn zen_toggle_flips_active_and_layout_region() {
    use crate::{mui_zen_active, mui_zen_toggle};
    // The Zen flag is a process-global (so `layout::region` is zen-aware
    // everywhere); serialize + restore so we don't disturb parallel layout tests.
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::layout::zen_active();
    crate::layout::set_zen(false);

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_zen_active(h), 0);
    let normal = crate::layout::region(true);

    // Toggle on: active + the editor region recomputes to the zen (chrome-hidden)
    // layout.
    assert_eq!(mui_zen_toggle(h), 1);
    assert_eq!(mui_zen_active(h), 1);
    let zen = crate::layout::region(true);
    assert!(zen.left < normal.left && zen.top < normal.top);

    // Toggle off restores.
    assert_eq!(mui_zen_toggle(h), 0);
    assert_eq!(mui_zen_active(h), 0);
    assert_eq!(crate::layout::region(true), normal);

    crate::layout::set_zen(before);
}

#[test]
fn workspace_open_reroots_tree_and_index_and_records_recent() {
    use crate::wsabi::{
        mui_ws_name_len, mui_ws_open, mui_ws_recent_count, mui_ws_root_len,
    };
    use crate::{mui_path_clear, mui_path_push, mui_quickopen_reindex};

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    // A temp folder with a couple of files to index.
    let root = std::env::temp_dir().join(format!("mui_ws_open_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("alpha.mty"), b"fn main() {}").unwrap();
    std::fs::write(root.join("beta.txt"), b"hello").unwrap();
    let root_str = root.to_string_lossy().into_owned();

    // Stage the folder path (byte buffer) + open it as the workspace.
    mui_path_clear(h);
    for b in root_str.bytes() {
        mui_path_push(h, b as u32);
    }
    assert_eq!(mui_ws_open(h), 1, "open of a valid folder should succeed");

    // The tree re-rooted there (its root drives the file list).
    assert_eq!(ctx.tree.root(), crate::workspace::validate_folder(&root_str).unwrap());
    // The workspace name + root are now non-empty.
    assert!(mui_ws_root_len(h) > 0);
    assert!(mui_ws_name_len(h) > 0);
    // The Quick-Open index re-rooted at the workspace finds both files.
    assert_eq!(mui_quickopen_reindex(h), 2, "index should re-root + see 2 files");
    // The folder was recorded in the recents MRU.
    assert_eq!(mui_ws_recent_count(h), 1);

    // Opening a non-existent folder fails (and doesn't grow recents).
    mui_path_clear(h);
    for b in root.join("nope-missing").to_string_lossy().bytes() {
        mui_path_push(h, b as u32);
    }
    assert_eq!(mui_ws_open(h), 0, "missing folder should fail");
    assert_eq!(mui_ws_recent_count(h), 1, "failed open must not record a recent");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn workspace_open_dialog_env_pick_reroots_tree_and_records_recent() {
    use crate::wsabi::{
        mui_ws_name_len, mui_ws_open_dialog, mui_ws_recent_count, mui_ws_root_len,
    };
    use crate::mui_quickopen_reindex;

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let root = std::env::temp_dir().join(format!("mui_ws_dialog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("main.mty"), b"fn main() {}").unwrap();
    std::fs::write(root.join("src").join("lib.mty"), b"fn lib() {}").unwrap();
    let root_str = root.to_string_lossy().into_owned();

    std::env::set_var("MUI_OPEN_FOLDER_PICK", &root_str);
    let opened = mui_ws_open_dialog(h);
    std::env::remove_var("MUI_OPEN_FOLDER_PICK");

    assert_eq!(opened, 1, "dialog pick of a valid folder should succeed");
    assert_eq!(ctx.tree.root(), crate::workspace::validate_folder(&root_str).unwrap());
    assert!(mui_ws_root_len(h) > 0);
    assert!(mui_ws_name_len(h) > 0);
    assert_eq!(mui_quickopen_reindex(h), 2, "index should see both files");
    assert_eq!(mui_ws_recent_count(h), 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_file_dialog_env_pick_opens_tab_and_records_recent() {
    use crate::{mui_open_file_dialog, mui_quickopen_reindex, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let root = std::env::temp_dir().join(format!("mui_open_file_dialog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let picked = root.join("picked.mty");
    std::fs::write(&picked, b"fn picked() -> I32 { 7 }").unwrap();

    std::env::set_var("MUI_OPEN_FILE_PICK", picked.to_string_lossy().as_ref());
    let idx = mui_open_file_dialog(h);
    std::env::remove_var("MUI_OPEN_FILE_PICK");

    assert_eq!(idx, 1, "dialog-picked file should open as a new tab");
    assert_eq!(mui_tab_count(h), 2);
    assert_eq!(mui_tab_active(h), 1);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(picked.as_path()));
    assert_eq!(ctx.tabs.active_model().as_text(), "fn picked() -> I32 { 7 }");
    assert_eq!(mui_quickopen_reindex(h), 1, "picked file's folder is still indexed");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn save_as_dialog_env_pick_writes_and_binds_untitled_tab() {
    use crate::{mui_active_has_path, mui_ed_dirty, mui_save_as_dialog};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::settings::active();
    crate::settings::set_active(crate::settings::Settings::default());

    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("fn main() {   ");
    ctx.tabs.set_dirty(ctx.tabs.active(), true);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_active_has_path(h), 0);
    assert_eq!(mui_ed_dirty(h), 1);

    let root = std::env::temp_dir().join(format!("mui_save_as_dialog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let target = root.join("saved.mty");

    std::env::set_var("MUI_SAVE_FILE_PICK", target.to_string_lossy().as_ref());
    let saved = mui_save_as_dialog(h);
    std::env::remove_var("MUI_SAVE_FILE_PICK");

    assert_eq!(saved, 0, "dialog-picked Save As should succeed");
    assert_eq!(mui_active_has_path(h), 1);
    assert_eq!(mui_ed_dirty(h), 0);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(target.as_path()));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {\n");

    crate::settings::set_active(before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lightbulb_visibility_and_click_open_actions() {
    use crate::ffi::MuiEvent;
    use crate::wsabi::{
        mui_lightbulb_click, mui_lightbulb_line, mui_lightbulb_reset, mui_lightbulb_visible,
    };

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    // No actions probed yet -> hidden, no line, no click hit.
    assert_eq!(mui_lightbulb_visible(h), 0);
    assert_eq!(mui_lightbulb_line(h), -1);
    assert_eq!(mui_lightbulb_click(h), 0);

    // Simulate a probe that found actions on the cursor's line (line 0 by
    // default for a fresh scratch buffer).
    let cursor = ctx.tabs.active_model().cursor_line() as i32;
    ctx.lightbulb.set_result(cursor, true);
    assert_eq!(mui_lightbulb_visible(h), 1, "bulb shows when actions exist for the line");
    assert_eq!(mui_lightbulb_line(h), cursor);

    // Draw it so its gutter rect is recorded, then a click on that rect hits.
    crate::wsabi::mui_lightbulb_draw(h, cursor, 1);
    let region = crate::layout::region(ctx.sidebar_visible);
    let cx = region.left + 8.0; // inside the bulb's ~17px-wide gutter slot
    let cy = crate::layout::row_y_in(region, cursor) + crate::layout::LINE_H() * 0.5;
    ctx.last_event = MuiEvent::none();
    ctx.last_event.x = cx;
    ctx.last_event.y = cy;
    assert_eq!(mui_lightbulb_click(h), 1, "a click on the drawn bulb should hit");

    // A click far away misses.
    ctx.last_event.x = cx + 400.0;
    assert_eq!(mui_lightbulb_click(h), 0);

    // No actions -> hidden even on the same line.
    ctx.lightbulb.set_result(cursor, false);
    assert_eq!(mui_lightbulb_visible(h), 0);

    // Reset clears everything.
    ctx.lightbulb.set_result(cursor, true);
    mui_lightbulb_reset(h);
    assert_eq!(mui_lightbulb_visible(h), 0);
}

#[test]
fn translate_close_and_resize_events() {
    let mut q = EventQueue::default();
    translate_window_event(&mut q, &winit::event::WindowEvent::CloseRequested);
    translate_window_event(
        &mut q,
        &winit::event::WindowEvent::Resized(winit::dpi::PhysicalSize::new(800, 600)),
    );
    assert_eq!(q.pop().unwrap().tag, MUI_EVENT_CLOSE);
    let r = q.pop().unwrap();
    assert_eq!(r.tag, MUI_EVENT_RESIZE);
    assert_eq!(r.width, 800);
    assert_eq!(r.height, 600);
    assert_eq!(q.pending_resize, Some((800, 600)));
}

/// `mui_headless_frames` returns 0 for a normal interactive run (no headless
/// env), and a positive cap when a headless/screenshot/probe env is set. Env
/// vars are process-global, so all cases run sequentially in one test with
/// careful cleanup (and the suite is single-threaded for env-touching tests).
#[test]
fn headless_frames_zero_without_env_positive_with_env() {
    use crate::abi::mui_headless_frames;

    // Clean any leftover headless env this test cares about so the baseline is
    // a true "interactive" launch.
    let keys = [
        "MUI_HEADLESS_FRAMES",
        "MUI_SCREENSHOT",
        "MUI_PALETTE_AUTOOPEN",
        "MUI_NAV_PROBE",
    ];
    for k in keys {
        std::env::remove_var(k);
    }

    // Interactive: no headless env -> run forever (0).
    assert_eq!(
        mui_headless_frames(),
        0,
        "no headless env should mean run-until-close (0)"
    );

    // Dedicated MUI_HEADLESS_FRAMES with a valid positive value -> that value.
    std::env::set_var("MUI_HEADLESS_FRAMES", "120");
    assert_eq!(mui_headless_frames(), 120);
    // Invalid / non-positive -> falls back to the default cap.
    std::env::set_var("MUI_HEADLESS_FRAMES", "notanumber");
    assert!(mui_headless_frames() > 0);
    std::env::set_var("MUI_HEADLESS_FRAMES", "0");
    assert!(mui_headless_frames() > 0);
    std::env::remove_var("MUI_HEADLESS_FRAMES");
    assert_eq!(mui_headless_frames(), 0);

    // Screenshot mode -> positive cap.
    std::env::set_var("MUI_SCREENSHOT", "out.png");
    assert!(mui_headless_frames() > 0);
    std::env::remove_var("MUI_SCREENSHOT");
    assert_eq!(mui_headless_frames(), 0);

    // Any *_AUTOOPEN screenshot hook -> positive cap.
    std::env::set_var("MUI_PALETTE_AUTOOPEN", "1");
    assert!(mui_headless_frames() > 0);
    std::env::remove_var("MUI_PALETTE_AUTOOPEN");
    assert_eq!(mui_headless_frames(), 0);

    // Any *_PROBE scripted probe -> positive cap.
    std::env::set_var("MUI_NAV_PROBE", "1");
    assert!(mui_headless_frames() > 0);
    std::env::remove_var("MUI_NAV_PROBE");
    assert_eq!(mui_headless_frames(), 0);
}

#[test]
fn mighty_enter_handlers_defer_to_single_command_dispatcher() {
    let main = include_str!("../../../src/main.mty");
    assert!(
        main.contains("command_click_id = mui_palette_selected_id(h)"),
        "palette Enter must queue the selected command id"
    );
    assert!(
        main.contains("command_click_id = mui_qo_command_id(h, -1)"),
        "Quick Open command mode must queue the selected command id"
    );
    assert_eq!(
        main.matches("if id == cmd_open_file()").count(),
        1,
        "command execution ladder must stay centralized"
    );
    assert!(
        main.contains("let applied = mui_codeaction_apply(h)"),
        "code action accept must inspect whether anything was actually applied"
    );
    assert!(
        !main.contains("let _a = mui_codeaction_apply(h)"),
        "code action accept must not blindly reload after a no-op action"
    );
}

/// Shim-side window-chrome + zoom interception (the v0.36-parser-safe move of the
/// title bar + zoom OUT of main.mty and INTO `mui_poll_event_s`). These drive
/// REAL winit `WindowEvent`s through `translate_window_event` into the live event
/// queue, then poll exactly as the IDE main loop does — so the same code path the
/// OS exercises is exercised here.
mod shim_chrome {
    use super::*;
    use crate::{
        mui_event_codepoint, mui_poll_event_s, mui_window_toggle_maximize, mui_zoom_reset,
    };
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};

    // Big enough that the title-bar controls (right edge) and interior are
    // distinct, and edges aren't the whole window.
    const WW: u32 = 1000;
    const WH: u32 = 700;

    fn handle(ctx: &mut MuiContext) -> i64 {
        (ctx as *mut MuiContext) as usize as i64
    }

    fn move_to(ctx: &mut MuiContext, x: f32, y: f32) {
        // winit reports PHYSICAL px; at ui_scale 1.0 logical == physical.
        let ev = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(x as f64, y as f64),
        };
        translate_window_event(&mut ctx.queue, &ev);
    }

    fn press_left(ctx: &mut MuiContext) {
        let ev = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        translate_window_event(&mut ctx.queue, &ev);
    }

    fn ctrl_down(ctx: &mut MuiContext) {
        // Emulate the modifier-state update winit pushes before the key/wheel.
        let mods = winit::keyboard::ModifiersState::CONTROL;
        let ev = WindowEvent::ModifiersChanged(mods.into());
        translate_window_event(&mut ctx.queue, &ev);
    }

    fn wheel(ctx: &mut MuiContext, dy: f32) {
        let ev = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, dy),
            phase: winit::event::TouchPhase::Moved,
        };
        translate_window_event(&mut ctx.queue, &ev);
    }

    #[test]
    fn close_button_press_is_delivered_as_close_event() {
        crate::uiscale::set_os_scale(1.0);
        crate::uiscale::set_user_zoom(1.0);
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => {
                eprintln!("skip: no GPU adapter");
                return;
            }
        };
        let h = handle(&mut ctx);
        // The close button is the rightmost ~46px of the title-bar row, y inside
        // the bar. Move there, then press.
        let cx = WW as f32 - crate::titlebar::BTN_W * 0.5;
        move_to(&mut ctx, cx, 8.0);
        press_left(&mut ctx);
        // The shim turns the close-button press into a real CLOSE the IDE handles.
        assert_eq!(mui_poll_event_s(h), MUI_EVENT_CLOSE as i32);
        // Nothing else queued.
        assert_eq!(mui_poll_event_s(h), 0);
    }

    #[test]
    fn min_max_drag_and_resize_presses_are_consumed_not_delivered() {
        crate::uiscale::set_os_scale(1.0);
        crate::uiscale::set_user_zoom(1.0);
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        let h = handle(&mut ctx);
        // Minimize button (leftmost of the three controls).
        let min_x = crate::titlebar::controls_x(WW as f32) + crate::titlebar::BTN_W * 0.5;
        move_to(&mut ctx, min_x, 8.0);
        press_left(&mut ctx);
        // Caption-strip drag region (between body_left and controls), still in bar.
        move_to(&mut ctx, WW as f32 * 0.5, 8.0);
        press_left(&mut ctx);
        // A resize edge (far right column, mid-height).
        move_to(&mut ctx, WW as f32 - 1.0, WH as f32 * 0.5);
        press_left(&mut ctx);
        // All three are window chrome -> consumed -> the IDE sees an empty queue.
        assert_eq!(
            mui_poll_event_s(h),
            0,
            "title-bar/resize presses must not reach the IDE"
        );
    }

    #[test]
    fn interior_press_passes_through_to_the_ide() {
        crate::uiscale::set_os_scale(1.0);
        crate::uiscale::set_user_zoom(1.0);
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        let h = handle(&mut ctx);
        // Deep in the editor body, well below the bar and off the edges.
        move_to(&mut ctx, WW as f32 * 0.5, WH as f32 * 0.5);
        press_left(&mut ctx);
        assert_eq!(
            mui_poll_event_s(h),
            MUI_EVENT_MOUSE_DOWN as i32,
            "an interior click must reach the IDE unchanged"
        );
    }

    #[test]
    fn ctrl_plus_minus_zero_chars_zoom_and_are_swallowed() {
        crate::uiscale::set_os_scale(1.0);
        crate::uiscale::set_user_zoom(1.0);
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        let h = handle(&mut ctx);
        mui_zoom_reset(h);
        ctrl_down(&mut ctx);
        // Ctrl+'=' twice (a char event with the Ctrl modifier folded in).
        for _ in 0..2 {
            ctx.queue.push(MuiEvent::char('=' as u32, MUI_MOD_CTRL));
        }
        // The IDE polls and sees NOTHING (both swallowed as zoom).
        assert_eq!(mui_poll_event_s(h), 0);
        assert!(
            (crate::uiscale::user_zoom() - 1.2).abs() < 0.001,
            "two Ctrl+= steps -> 1.2, got {}",
            crate::uiscale::user_zoom()
        );
        // Ctrl+'-' once -> back toward 1.1.
        ctx.queue.push(MuiEvent::char('-' as u32, MUI_MOD_CTRL));
        assert_eq!(mui_poll_event_s(h), 0);
        assert!((crate::uiscale::user_zoom() - 1.1).abs() < 0.001);
        // Ctrl+'0' resets.
        ctx.queue.push(MuiEvent::char('0' as u32, MUI_MOD_CTRL));
        assert_eq!(mui_poll_event_s(h), 0);
        assert!((crate::uiscale::user_zoom() - 1.0).abs() < 0.001);
        let _ = mui_event_codepoint(h); // no panic on the accessor
        crate::uiscale::set_user_zoom(1.0);
    }

    #[test]
    fn ctrl_wheel_zooms_plain_wheel_scrolls() {
        crate::uiscale::set_os_scale(1.0);
        crate::uiscale::set_user_zoom(1.0);
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        let h = handle(&mut ctx);
        mui_zoom_reset(h);
        // Ctrl+wheel-up -> zoom in, swallowed.
        ctrl_down(&mut ctx);
        wheel(&mut ctx, 1.0);
        assert_eq!(mui_poll_event_s(h), 0, "Ctrl+wheel must be swallowed");
        assert!(crate::uiscale::user_zoom() > 1.0, "Ctrl+wheel-up zoomed in");
        // A PLAIN wheel (no Ctrl) passes through as a normal scroll for the editor.
        let mods = winit::keyboard::ModifiersState::empty();
        translate_window_event(
            &mut ctx.queue,
            &WindowEvent::ModifiersChanged(mods.into()),
        );
        wheel(&mut ctx, -1.0);
        assert_eq!(
            mui_poll_event_s(h),
            MUI_EVENT_SCROLL as i32,
            "a plain wheel must reach the IDE as a scroll"
        );
        crate::uiscale::set_user_zoom(1.0);
    }

    #[test]
    fn typed_char_without_ctrl_reaches_the_ide() {
        crate::uiscale::set_os_scale(1.0);
        crate::uiscale::set_user_zoom(1.0);
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        let h = handle(&mut ctx);
        // A normal typed 'h' (no modifiers) must NOT be swallowed.
        ctx.queue.push(MuiEvent::char('h' as u32, 0));
        assert_eq!(mui_poll_event_s(h), MUI_EVENT_CHAR as i32);
        assert_eq!(mui_event_codepoint(h), 'h' as i32);
        let _ = mui_window_toggle_maximize(h); // host is None -> no-op, no panic
    }
}
