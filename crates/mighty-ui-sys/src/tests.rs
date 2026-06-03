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

#[test]
fn initial_tree_root_uses_file_parent_when_file_is_provided() {
    let root = std::env::temp_dir().join(format!("mui_initial_tree_file_{}", std::process::id()));
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("main.mty");
    let cwd = root.join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();

    let got = crate::initial_tree_root_for(Some(&file), Some(cwd), None);
    assert_eq!(got, src);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn initial_tree_root_prefers_packaged_samples_for_no_arg_launch() {
    let root = std::env::temp_dir().join(format!("mui_initial_tree_samples_{}", std::process::id()));
    let exe_dir = root.join("dist");
    let samples = exe_dir.join("samples");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&samples).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(exe_dir.join("mighty-ide.exe"), b"").unwrap();

    let got = crate::initial_tree_root_for(None, Some(cwd), Some(exe_dir));
    assert_eq!(got, samples);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn initial_tree_root_keeps_cwd_for_dev_no_arg_launch() {
    let root = std::env::temp_dir().join(format!("mui_initial_tree_cwd_{}", std::process::id()));
    let exe_dir = root.join("target").join("release");
    let samples = exe_dir.join("samples");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&samples).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let got = crate::initial_tree_root_for(None, Some(cwd.clone()), Some(exe_dir));
    assert_eq!(got, cwd);

    let _ = std::fs::remove_dir_all(root);
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
fn multi_cursor_edge_commands_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.tabs
        .active_model_mut()
        .set_text_preserving_cursor("unique stuff here");
    ctx.tabs.active_model_mut().move_to(0, 0);
    assert_eq!(crate::abi::mui_ed_add_caret_next(handle), 1);
    assert_eq!(ctx.toasts.toasts().len(), 0, "successful Ctrl+D should stay quiet");
    assert_eq!(crate::abi::mui_ed_add_caret_next(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No word or next occurrence for multi-cursor");

    ctx.tabs
        .active_model_mut()
        .set_text_preserving_cursor("top\nbottom");
    ctx.tabs.active_model_mut().move_to(0, 0);
    assert_eq!(crate::abi::mui_ed_add_caret_above(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No line above for another caret");

    let before_success = ctx.toasts.toasts().len();
    assert_eq!(crate::abi::mui_ed_add_caret_below(handle), 1);
    assert_eq!(
        ctx.toasts.toasts().len(),
        before_success,
        "successful vertical caret addition should stay quiet"
    );

    ctx.tabs.active_model_mut().collapse_carets();
    ctx.tabs.active_model_mut().move_to(1, 0);
    assert_eq!(crate::abi::mui_ed_add_caret_below(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No line below for another caret");
}

#[test]
fn jump_back_empty_target_toast_is_available_to_mighty_dispatch() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::abi::mui_toast(handle, 0, 10);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No previous location");
}

#[test]
fn fold_commands_report_empty_and_noop_outcomes() {
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.tabs
        .active_model_mut()
        .set_text_preserving_cursor("let x = 1\nlet y = 2");
    ctx.tabs.recompute_active_fold();
    assert_eq!(
        crate::abi::mui_fold_dispatch(handle, crate::palette::CMD_FOLD_TOGGLE as i32),
        0
    );
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No foldable block at cursor");

    assert_eq!(
        crate::abi::mui_fold_dispatch(handle, crate::palette::CMD_FOLD_ALL as i32),
        0
    );
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "No foldable blocks");

    ctx.toasts.clear();
    ctx.tabs
        .active_model_mut()
        .set_text_preserving_cursor("fn main() {\n  let x = 1\n}\n\nlet y = 2");
    ctx.tabs.active_model_mut().move_to(0, 0);
    ctx.tabs.recompute_active_fold();
    assert_eq!(
        crate::abi::mui_fold_dispatch(handle, crate::palette::CMD_FOLD_TOGGLE as i32),
        1
    );
    assert!(ctx.toasts.toasts().is_empty(), "successful fold toggle should stay quiet");

    assert_eq!(
        crate::abi::mui_fold_dispatch(handle, crate::palette::CMD_FOLD_ALL as i32),
        0
    );
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "All foldable blocks already folded"
    );

    ctx.toasts.clear();
    assert_eq!(
        crate::abi::mui_fold_dispatch(handle, crate::palette::CMD_UNFOLD_ALL as i32),
        1
    );
    assert!(ctx.toasts.toasts().is_empty(), "successful unfold all should stay quiet");

    assert_eq!(
        crate::abi::mui_fold_dispatch(handle, crate::palette::CMD_UNFOLD_ALL as i32),
        0
    );
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "No folded blocks to unfold"
    );
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
fn text_measure_sized_tracks_requested_size() {
    let mut ctx = ctx_or_skip!();
    let sig = "fn add(a: I32, b: I32) -> I32";
    let (small_w, small_h) = ctx.text.measure_sized(sig, 12.0);
    let (large_w, large_h) = ctx.text.measure_sized(sig, 18.0);

    assert!(small_w > 0.0, "small width should be > 0, got {small_w}");
    assert!(small_h > 0.0, "small height should be > 0, got {small_h}");
    assert!(
        large_w > small_w,
        "larger text should measure wider: small={small_w}, large={large_w}"
    );
    assert!(
        large_h > small_h,
        "larger text should measure taller: small={small_h}, large={large_h}"
    );
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
        mui_dirty_confirm_active, mui_dirty_confirm_cancel, mui_dirty_confirm_click, mui_dirty_confirm_discard,
        mui_dirty_confirm_save,
        mui_ed_set_dirty, mui_path_clear, mui_path_push, mui_quit_request, mui_tab_active,
        mui_tab_close, mui_tab_count, mui_tab_cursor_col, mui_tab_cursor_line, mui_tab_load,
        mui_tab_load_byte, mui_tab_open_path, mui_tab_scroll, mui_tab_set_dirty,
        mui_tab_store_begin, mui_tab_store_byte, mui_tab_store_commit, mui_tab_switch,
    };

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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
    assert!(ctx.path_stage.is_empty());
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
    assert!(ctx.path_stage.is_empty());
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);
    mui_path_clear(handle);

    assert_eq!(mui_tab_switch(handle, 99), -1);
    assert_eq!(mui_tab_active(handle), 1);
    assert_eq!(mui_tab_count(handle), 2);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No tab at that position");
    assert_eq!(mui_tab_close(handle, 99), -1);
    assert_eq!(mui_tab_active(handle), 1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "No tab at that position"
    );

    // The confirmation overlay can save a dirty file-backed tab before closing.
    let save_path = dir.join("mui_tababi_save_confirm.txt");
    std::fs::write(&save_path, b"save me").unwrap();
    for b in save_path.to_string_lossy().as_bytes() {
        mui_path_push(handle, *b as u32);
    }
    assert_eq!(mui_tab_open_path(handle), 2);
    mui_tab_set_dirty(handle, 2, 1);
    assert_eq!(mui_tab_close(handle, 2), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(
        toast.message,
        "Review unsaved changes in mui_tababi_save_confirm.txt"
    );
    assert_eq!(mui_dirty_confirm_save(handle), 1);
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);
    mui_path_clear(handle);

    // Dirty tabs require an explicit confirmation choice before closing.
    mui_tab_set_dirty(handle, 1, 1);
    assert_eq!(mui_quit_request(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Review 1 unsaved tab before quitting");
    assert_eq!(mui_dirty_confirm_active(handle), 1);
    mui_dirty_confirm_cancel(handle);
    assert_eq!(mui_dirty_confirm_active(handle), 0);
    assert_eq!(mui_quit_request(handle), 0);
    assert_eq!(mui_quit_request(handle), 0, "repeat quit should keep the modal active");
    mui_dirty_confirm_cancel(handle);
    assert_eq!(mui_tab_close(handle, 1), -1);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Review unsaved changes in mui_tababi_open.txt"
    );
    assert_eq!(mui_tab_close(handle, 1), -1, "repeat close should not discard");
    assert_eq!(mui_dirty_confirm_active(handle), 1);

    // Scaled-window paths can have raw GPU dimensions wider/taller than the
    // logical event space. The modal hit boxes must use the same visible logical
    // dimensions as mouse events, or the buttons drift and miss.
    ctx.gpu.width = 1280;
    ctx.gpu.height = 832;
    ctx.gpu.phys_width = 1280;
    ctx.gpu.phys_height = 832;
    crate::uiscale::set_os_scale(1.375);
    crate::uiscale::set_user_zoom(1.0);
    let visible_w = crate::layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width) as f32;
    let visible_h = crate::layout::visible_height(ctx.gpu.height, ctx.gpu.phys_height) as f32;
    let (card_x, card_y, card_w, _card_h) = crate::abi::dirty_confirm_card_rect(visible_w, visible_h);
    let btn_w = crate::abi::dirty_confirm_button_width(card_w);
    let btn_h = 34.0;
    let by = card_y + _card_h - 54.0;
    let discard_x = card_x + card_w - btn_w - 24.0;
    let save_x = discard_x - btn_w - 12.0;
    let cancel_x = save_x - btn_w - 12.0;
    ctx.last_event = crate::ffi::MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        cancel_x + btn_w * 0.5,
        by + btn_h * 0.5,
        0,
    );
    assert_eq!(mui_dirty_confirm_click(handle), 1);
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);

    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), 1);
    mui_dirty_confirm_cancel(handle);

    let long_detail = "extremely_long_component_name_that_used_to_run_across_the_dialog_and_overlap_buttons.mty has unsaved edits. Discarding cannot be undone.";
    let fitted_detail = crate::abi::fit_dirty_confirm_detail(
        &mut ctx.text,
        long_detail,
        360.0,
        crate::theme::CHROME_FONT_SIZE,
    );
    let (fitted_w, _) = ctx
        .text
        .measure_ui_sized(&fitted_detail, crate::theme::CHROME_FONT_SIZE);
    assert!(fitted_w <= 312.0, "dirty-confirm detail should fit modal text budget: {fitted_detail}");
    assert!(
        fitted_detail.ends_with("cannot be undone."),
        "tail should preserve the consequence text: {fitted_detail}"
    );
    let compact_card_w = 288.0;
    let compact_btn_w = crate::abi::dirty_confirm_button_width(compact_card_w);
    let button_row_w = compact_btn_w * 3.0 + 24.0;
    assert!(
        button_row_w <= compact_card_w - 48.0,
        "dirty-confirm buttons should fit compact card: row={button_row_w} card={compact_card_w}"
    );
    for label in ["Cancel", "Save", "Discard"] {
        let fitted = crate::abi::fit_dirty_confirm_button_label(
            &mut ctx.text,
            label,
            compact_btn_w,
            crate::theme::CHROME_FONT_SIZE,
        );
        assert_eq!(fitted, label);
        let (label_w, _) = ctx
            .text
            .measure_ui_sized(&fitted, crate::theme::CHROME_FONT_SIZE);
        assert!(
            label_w + 12.0 <= compact_btn_w,
            "button label should fit centered compact button: {label}"
        );
    }
    let (tiny_x, _tiny_y, tiny_card_w, _tiny_card_h) = crate::abi::dirty_confirm_card_rect(180.0, 360.0);
    assert!(tiny_x >= 0.0);
    assert!(tiny_card_w <= 180.0);
    assert!(tiny_x + tiny_card_w <= 180.0 + 0.5);
    let tiny_btn_w = crate::abi::dirty_confirm_button_width(tiny_card_w);
    assert!(
        tiny_btn_w * 3.0 + 24.0 <= tiny_card_w - 48.0 + 0.5,
        "dirty-confirm buttons should shrink inside tiny card"
    );
    let (_short_x, short_y, _short_w, short_h) = crate::abi::dirty_confirm_card_rect(520.0, 220.0);
    assert!(short_y >= 0.0);
    assert!(short_y + short_h <= 220.0 + 0.5);
    let long_label = "Discard changes permanently";
    let fitted_long_label = crate::abi::fit_dirty_confirm_button_label(
        &mut ctx.text,
        long_label,
        compact_btn_w,
        crate::theme::CHROME_FONT_SIZE,
    );
    let (long_label_w, _) = ctx
        .text
        .measure_ui_sized(&fitted_long_label, crate::theme::CHROME_FONT_SIZE);
    assert!(
        long_label_w + 12.0 <= compact_btn_w + 0.5,
        "long button label should fit compact button: {fitted_long_label}"
    );
    assert!(
        fitted_long_label.ends_with('…') || fitted_long_label.len() < long_label.len(),
        "long button label should be visibly shortened: {fitted_long_label}"
    );

    // Dirty untitled tabs keep the confirmation active and explain a cancelled
    // Save dialog instead of failing silently.
    let untitled = ctx.tabs.new_untitled();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("scratch");
    mui_tab_set_dirty(handle, untitled as i32, 1);
    assert_eq!(mui_tab_close(handle, untitled as i32), -1);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Review unsaved changes in (scratch)"
    );
    std::env::set_var("MUI_SAVE_FILE_PICK", "");
    assert_eq!(mui_dirty_confirm_save(handle), -3);
    std::env::remove_var("MUI_SAVE_FILE_PICK");
    assert_eq!(mui_dirty_confirm_active(handle), 1);
    assert_eq!(mui_tab_count(handle), 3);
    assert!(ctx.tabs.is_dirty(untitled));
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Save cancelled; tab is still open"
    );
    mui_dirty_confirm_cancel(handle);
    mui_tab_set_dirty(handle, untitled as i32, 0);
    assert!(mui_tab_close(handle, untitled as i32) >= 0);
    assert_eq!(mui_tab_count(handle), 2);

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

    // Dirty close opens the modal; only explicit Discard closes the tab.
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
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::ffi::MuiEvent;
    use crate::{
        mui_rail_utility_at_click, mui_tab_close_index_at_click, mui_tab_index_at_click,
        mui_tree_row_at_click, mui_window_resize_at_click,
    };
    use crate::layout;
    use crate::panels::mui_ai_click;

    let mut ctx = ctx_or_skip!();
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
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
    let body_left = layout::sidebar_right();
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
    let ai_reserved_x = crate::titlebar::controls_x(ctx.gpu.width as f32)
        - crate::titlebar::ACTION_STRIP_W
        + 4.0;
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, ai_reserved_x, 4.0, 0);
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
    ctx.last_event.x = layout::sidebar_right() + 100.0;
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
    assert_eq!(
        mui_window_resize_at_click(handle),
        0,
        "rail Settings click must not be swallowed by the southwest resize corner"
    );

    // The right-docked AI panel owns its surface, including the send affordance,
    // while still leaving the top-right chrome strip to title-bar actions.
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    let reserved_x = crate::titlebar::controls_x(ctx.gpu.width as f32)
        - crate::titlebar::ACTION_STRIP_W
        + 4.0;
    ctx.ai.open = true;
    ctx.ai.input = "ship it".to_string();
    let ai_visible_w = layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width);
    let ai_visible_h = layout::visible_height(ctx.gpu.height, ctx.gpu.phys_height);
    let ai_input = ctx.ai.input.clone();
    let (px, pw, input_y, input_h) =
        crate::ai::input_geometry(&mut ctx.text, &ai_input, ai_visible_w, ai_visible_h);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        px + pw - 24.0,
        input_y + input_h - 20.0,
        0,
    );
    assert_eq!(mui_ai_click(handle), 2);
    let (close_x, close_y, close_w, close_h) = crate::ai::close_geometry(ai_visible_w);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        close_x + close_w * 0.5,
        close_y + close_h * 0.5,
        0,
    );
    assert_eq!(mui_ai_click(handle), 3);
    ctx.last_event.x = px + 24.0;
    ctx.last_event.y = input_y + 12.0;
    assert_eq!(mui_ai_click(handle), 1);
    ctx.last_event.x = px - 2.0;
    assert_eq!(mui_ai_click(handle), 0);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, reserved_x, 4.0, 0);
    assert_eq!(mui_ai_click(handle), 0);

    // DPI/capture paths can report a logical GPU width wider than the actual
    // visible surface. The AI drawer must anchor to the same visible width used
    // by bottom docks, or the right edge of chat text renders off-screen. The
    // send hit-test must also use visible height, or the bottom composer icon
    // misses under scaled Windows coordinates.
    ctx.gpu.width = 1374;
    ctx.gpu.phys_width = 1280;
    ctx.gpu.height = 832;
    ctx.gpu.phys_height = 832;
    crate::uiscale::set_os_scale(1.25);
    crate::uiscale::set_user_zoom(1.0);
    ctx.ai.open = true;
    ctx.ai.input = "send from scaled window".to_string();
    let visible_w = layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width);
    let visible_h = layout::visible_height(ctx.gpu.height, ctx.gpu.phys_height);
    let ai_input = ctx.ai.input.clone();
    let (visible_px, visible_pw, visible_input_y, visible_input_h) =
        crate::ai::input_geometry(&mut ctx.text, &ai_input, visible_w, visible_h);
    let (raw_px, _raw_pw, raw_input_y, _raw_input_h) =
        crate::ai::input_geometry(&mut ctx.text, &ai_input, ctx.gpu.width, ctx.gpu.height);
    assert!(
        raw_px > visible_px,
        "raw logical width would push the drawer off the captured surface"
    );
    assert!(
        raw_input_y > visible_input_y,
        "raw GPU height would push the send hit target below the visible composer"
    );
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        visible_px + visible_pw - 24.0,
        visible_input_y + visible_input_h - 20.0,
        0,
    );
    assert_eq!(mui_ai_click(handle), 2);

    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn prompt_hit_test_tracks_visible_bottom_band() {
    use crate::ffi::MuiEvent;
    use crate::{mui_prompt_close_at_click, mui_prompt_hit_at_click, prompt::PromptKind};

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    ctx.prompt.open(PromptKind::Open as i32);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let band_y = ctx.gpu.height as f32 - 30.0 - crate::layout::LINE_H();
    let close_size = (crate::layout::LINE_H() - 6.0).clamp(18.0, 24.0);
    let close_x = ctx.gpu.width as f32 - close_size - 8.0;
    let close_y = band_y + (crate::layout::LINE_H() - close_size) * 0.5;

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, 450.0, band_y + 4.0, 0);
    assert_eq!(mui_prompt_hit_at_click(handle), 1);

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        close_x + close_size * 0.5,
        close_y + close_size * 0.5,
        0,
    );
    assert_eq!(mui_prompt_close_at_click(handle), 1);

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        close_x - 8.0,
        close_y + close_size * 0.5,
        0,
    );
    assert_eq!(mui_prompt_close_at_click(handle), 0);

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, 450.0, band_y - 4.0, 0);
    assert_eq!(mui_prompt_hit_at_click(handle), 0);

    ctx.prompt.cancel();
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, 450.0, band_y + 4.0, 0);
    assert_eq!(mui_prompt_hit_at_click(handle), 0);
}

#[test]
fn replace_bar_close_hit_tracks_visible_button() {
    use crate::ffi::MuiEvent;
    use crate::mui_replace_close_at_click;

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    ctx.replace_bar.open("needle");
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let bar_h = crate::layout::LINE_H();
    let top = ctx.gpu.height as f32 - 30.0 - 2.0 * bar_h;
    let close_size = (bar_h - 6.0).clamp(18.0, 24.0);
    let close_x = ctx.gpu.width as f32 - close_size - 8.0;
    let close_y = top + 4.0;

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        close_x + close_size * 0.5,
        close_y + close_size * 0.5,
        0,
    );
    assert_eq!(mui_replace_close_at_click(handle), 1);

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        close_x - 10.0,
        close_y + close_size * 0.5,
        0,
    );
    assert_eq!(mui_replace_close_at_click(handle), 0);

    ctx.replace_bar.cancel();
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        close_x + close_size * 0.5,
        close_y + close_size * 0.5,
        0,
    );
    assert_eq!(mui_replace_close_at_click(handle), 0);
}

#[test]
fn tab_bar_long_dirty_label_keeps_close_affordance_clickable() {
    use crate::{mui_tab_bar_draw, mui_tab_close_index_at_click};

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 720;
    ctx.gpu.height = 480;
    ctx.sidebar_visible = false;
    let path = std::env::temp_dir().join(
        "mighty_tab_label_with_a_very_long_filename_that_must_not_overlap_close_icon.mty",
    );
    let _ = std::fs::write(&path, b"fn main() -> I32 { 1 }\n");
    let tab = ctx.tabs.open_path(path);
    ctx.tabs.get_mut(tab).unwrap().dirty = true;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    mui_tab_bar_draw(handle);

    let body_left = crate::layout::body_left(false);
    ctx.last_event = crate::ffi::MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        body_left + tab as f32 * crate::layout::TAB_W + crate::layout::TAB_W - 18.0,
        crate::layout::TAB_BAR_H * 0.5,
        0,
    );
    assert_eq!(mui_tab_close_index_at_click(handle), tab as i32);
}

#[test]
fn tab_label_truncation_keeps_filename_start_and_extension() {
    let mut ctx = ctx_or_skip!();
    let label = crate::abi::fit_tab_label(&mut ctx.text, "harnesswelcome.mty", 104.0, 14.0);
    assert!(
        label.starts_with("harness"),
        "tab labels should preserve the basename start, got `{label}`"
    );
    assert!(
        label.ends_with("mty"),
        "tab labels should preserve the language extension, got `{label}`"
    );
    assert!(
        !label.contains("\u{2026}."),
        "truncated tab labels should avoid ellipsis-plus-dot visual stutter, got `{label}`"
    );
    assert!(
        label.contains('\u{2026}'),
        "narrow labels should truncate with an ellipsis, got `{label}`"
    );
    assert!(
        !label.starts_with('\u{2026}'),
        "tab labels should not lead with an ellipsis when the basename start fits"
    );
}

#[test]
fn command_surface_text_fits_before_shortcut_chrome() {
    let mut ctx = ctx_or_skip!();
    let long = "File: Close Saved Tabs to the Right";
    let fitted = crate::palette::fit_palette_text(&mut ctx.text, long, 136.0, 13.5);
    assert!(
        fitted.ends_with('\u{2026}'),
        "long command labels should visibly truncate at the row text boundary, got `{fitted}`"
    );
    assert!(
        fitted.starts_with("File:"),
        "command label fitting should preserve the command family prefix, got `{fitted}`"
    );
    assert!(
        ctx.text.measure_ui_sized(&fitted, 13.5).0 <= 136.0,
        "fitted command label must not draw under shortcut chrome"
    );
}

#[test]
fn tab_bar_overflow_scroll_maps_visible_slots_to_real_tabs() {
    use crate::ffi::MuiEvent;
    use crate::{mui_tab_bar_draw, mui_tab_close_index_at_click, mui_tab_index_at_click};

    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.gpu.width = 640;
    ctx.gpu.phys_width = 640;
    crate::layout::set_window_width(640);
    ctx.tabs.ensure_scratch();
    let root = std::env::temp_dir().join(format!("mui_tab_overflow_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..6 {
        let p = root.join(format!("tab-{i}.mty"));
        std::fs::write(&p, format!("fn tab_{i}() {{}}\n")).unwrap();
        ctx.tabs.open_path(p);
    }
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    mui_tab_bar_draw(handle);
    assert!(ctx.tab_scroll > 0, "active overflow tab should be scrolled into view");
    let first_visible = ctx.tab_scroll;
    let body_left = crate::layout::body_left(ctx.sidebar_visible);

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        body_left + 8.0,
        crate::layout::TAB_BAR_H * 0.5,
        0,
    );
    assert_eq!(mui_tab_index_at_click(handle), first_visible as i32);

    ctx.last_event.x = body_left + crate::layout::TAB_W - 20.0;
    assert_eq!(mui_tab_close_index_at_click(handle), first_visible as i32);

    crate::layout::set_window_width(900);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn activity_rail_all_slots_are_click_targets() {
    use crate::ffi::MuiEvent;
    use crate::panels::mui_rail_panel_at_click;

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    let cell = 38.0_f32;
    let gap = 4.0_f32;
    let icon_top = 52.0_f32;
    for slot in 0..=8 {
        let y = icon_top + slot as f32 * (cell + gap) + cell * 0.5;
        ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, 26.0, y, 0);
        assert_eq!(mui_rail_panel_at_click(handle), slot);
    }

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, 26.0, icon_top - 1.0, 0);
    assert_eq!(mui_rail_panel_at_click(handle), -1);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, crate::layout::RAIL_W + 1.0, icon_top, 0);
    assert_eq!(mui_rail_panel_at_click(handle), -1);
}

#[test]
fn view_commands_open_non_sidebar_surfaces_without_toggling() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_run_open(handle), 1);
    assert_eq!(crate::featureabi::mui_run_active(handle), 1);
    assert_eq!(crate::featureabi::mui_run_open(handle), 1);
    assert_eq!(crate::featureabi::mui_run_active(handle), 1);

    assert_eq!(crate::webabi::mui_web_open(handle), 1);
    assert_eq!(crate::webabi::mui_web_active(handle), 1);
    assert_eq!(crate::featureabi::mui_run_active(handle), 0);
    assert_eq!(crate::featureabi::mui_run_toggle(handle), 1);
    assert_eq!(crate::featureabi::mui_run_active(handle), 1);
    assert_eq!(crate::webabi::mui_web_active(handle), 0);
    assert_eq!(crate::featureabi::mui_run_toggle(handle), 0);
    assert_eq!(crate::featureabi::mui_run_active(handle), 0);
    assert_eq!(crate::webabi::mui_web_open(handle), 1);
    assert_eq!(crate::webabi::mui_web_active(handle), 1);

    assert_eq!(crate::navsurfaces::mui_problems_open(handle), 1);
    assert_eq!(crate::navsurfaces::mui_problems_is_open(handle), 1);
    assert_eq!(crate::webabi::mui_web_active(handle), 0);
    assert_eq!(crate::featureabi::mui_run_open(handle), 1);
    assert_eq!(crate::featureabi::mui_run_active(handle), 1);
    assert_eq!(crate::navsurfaces::mui_problems_is_open(handle), 0);

    assert_eq!(crate::panels::mui_ai_show(handle), 1);
    assert_eq!(crate::panels::mui_ai_is_open(handle), 1);
    assert_eq!(crate::panels::mui_ai_show(handle), 1);
    assert_eq!(crate::panels::mui_ai_is_open(handle), 1);
    assert_eq!(crate::panels::mui_ai_close(handle), 1);
    assert_eq!(crate::panels::mui_ai_is_open(handle), 0);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "AI Copilot closed");
    assert_eq!(crate::panels::mui_ai_close(handle), 0);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "AI Copilot is already closed"
    );
}

#[test]
fn web_playground_idle_controls_explain_noops() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::webabi::mui_web_stop(handle);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No web server running");

    assert_eq!(crate::webabi::mui_web_open_browser(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Web URL not ready");
}

#[test]
fn web_clear_output_reports_feedback_and_preserves_url() {
    let mut ctx = ctx_or_skip!();
    ctx.term_open = true;
    ctx.run.open();
    ctx.problems.set_open(true);
    ctx.web.seed_demo("examples/webspin/src/main.mty");
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::webabi::mui_web_clear(handle), 6);
    assert!(ctx.web.is_active());
    assert_eq!(ctx.web.line_count(), 0);
    assert_eq!(ctx.web.url(), "http://127.0.0.1:8000");
    assert!(ctx.web.is_running());
    assert!(!ctx.term_open);
    assert!(!ctx.run.is_active());
    assert!(!ctx.problems.is_open());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Web output cleared");

    assert_eq!(crate::webabi::mui_web_clear(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Web output already empty");
}

#[test]
fn web_headless_open_browser_does_not_cover_screenshot_with_success_toast() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old_screenshot = std::env::var_os("MUI_SCREENSHOT");
    std::env::set_var("MUI_SCREENSHOT", "target/web-headless-open-browser.png");

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    ctx.web.seed_demo("examples/webspin/src/main.mty");

    let before = ctx.toasts.toasts().len();
    assert_eq!(crate::webabi::mui_web_open_browser(handle), 1);
    assert_eq!(ctx.toasts.toasts().len(), before);

    if let Some(v) = old_screenshot {
        std::env::set_var("MUI_SCREENSHOT", v);
    } else {
        std::env::remove_var("MUI_SCREENSHOT");
    }
}

#[test]
fn ai_send_idle_controls_explain_noops() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
    let old_claude = std::env::var_os("CLAUDE_API_KEY");
    std::env::remove_var("ANTHROPIC_API_KEY");
    std::env::remove_var("CLAUDE_API_KEY");

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.ai.input = "   ".to_string();
    assert_eq!(crate::panels::mui_ai_send(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Type a message before sending");

    ctx.ai.input = "why did this fail?".to_string();
    assert_eq!(crate::panels::mui_ai_send(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Set ANTHROPIC_API_KEY to enable AI Copilot");
    assert_eq!(ctx.ai.input, "why did this fail?");

    if let Some(v) = old_anthropic {
        std::env::set_var("ANTHROPIC_API_KEY", v);
    }
    if let Some(v) = old_claude {
        std::env::set_var("CLAUDE_API_KEY", v);
    }
}

#[test]
fn ai_inline_send_reports_unavailable_outcomes() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
    let old_claude = std::env::var_os("CLAUDE_API_KEY");
    std::env::remove_var("ANTHROPIC_API_KEY");
    std::env::remove_var("CLAUDE_API_KEY");

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.ai.open = false;
    ctx.ai.input = "explain the selected code".to_string();
    assert_eq!(crate::panels::mui_ai_send_inline(handle), 0);
    assert!(ctx.ai.open);
    assert_eq!(ctx.ai.input, "explain the selected code");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Set ANTHROPIC_API_KEY to enable AI Copilot");

    if let Some(v) = old_anthropic {
        std::env::set_var("ANTHROPIC_API_KEY", v);
    }
    if let Some(v) = old_claude {
        std::env::set_var("CLAUDE_API_KEY", v);
    }
}

#[test]
fn ai_clear_chat_reports_state_and_resets_panel() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.ai.open = false;
    ctx.ai.input = "draft".to_string();
    ctx.ai.scroll = 48.0;
    ctx.ai.transcript.push(crate::ai::Turn {
        role: crate::ai::Role::User,
        text: "question".to_string(),
    });
    ctx.ai.transcript.push(crate::ai::Turn {
        role: crate::ai::Role::Assistant,
        text: "answer".to_string(),
    });

    assert_eq!(crate::panels::mui_ai_clear(handle), 1);
    assert!(ctx.ai.open);
    assert!(ctx.ai.input.is_empty());
    assert!(ctx.ai.transcript.is_empty());
    assert_eq!(ctx.ai.scroll, 0.0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "AI Copilot chat cleared");

    assert_eq!(crate::panels::mui_ai_clear(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "AI Copilot chat is already empty");
}

#[test]
fn sidebar_layout_commands_open_and_resize_sidebar() {
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 1200;
    ctx.sidebar_visible = false;
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(1200);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::layout::sidebar_w(), crate::layout::SIDEBAR_W);
    assert_eq!(
        crate::abi::mui_sidebar_layout_dispatch(handle, crate::palette::CMD_SIDEBAR_COMPACT as i32),
        1
    );
    assert!(ctx.sidebar_visible);
    assert_eq!(ctx.active_panel, crate::PANEL_EXPLORER);
    assert_eq!(crate::layout::sidebar_preset(), 1);
    assert_eq!(crate::layout::sidebar_w(), crate::layout::SIDEBAR_MIN_W);

    assert_eq!(
        crate::abi::mui_sidebar_layout_dispatch(handle, crate::palette::CMD_SIDEBAR_WIDE as i32),
        3
    );
    assert_eq!(crate::layout::sidebar_preset(), 2);
    assert_eq!(crate::layout::sidebar_w(), 360.0);

    assert_eq!(
        crate::abi::mui_sidebar_layout_dispatch(handle, crate::palette::CMD_SIDEBAR_DEFAULT as i32),
        2
    );
    assert_eq!(crate::layout::sidebar_preset(), 0);
    assert_eq!(crate::layout::sidebar_w(), crate::layout::SIDEBAR_W);
    crate::layout::reset_sidebar_preset();
}

#[test]
fn sidebar_toggle_acknowledges_open_and_close() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.sidebar_visible = true;
    assert_eq!(crate::abi::mui_sidebar_toggle(handle), 0);
    assert!(!ctx.sidebar_visible);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Sidebar closed"
    );

    assert_eq!(crate::abi::mui_sidebar_toggle(handle), 1);
    assert!(ctx.sidebar_visible);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Sidebar opened"
    );
}

#[test]
fn sidebar_resize_visible_grip_stays_subtle() {
    assert_eq!(crate::abi::sidebar_resize_grip_height(700.0), 42.0);
    assert_eq!(crate::abi::sidebar_resize_grip_height(72.0), 24.0);
    assert_eq!(crate::abi::sidebar_resize_grip_height(24.0), 18.0);
}

#[test]
fn sidebar_resize_preserves_grab_offset_inside_hit_band() {
    use crate::ffi::MuiEvent;

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 1200;
    ctx.gpu.height = 700;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    ctx.sidebar_visible = true;
    let zen_before = crate::layout::zen_active();
    crate::layout::set_zen(false);
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(1200);
    let start_w = crate::layout::sidebar_w();
    let right = crate::layout::sidebar_right();
    let (_, visible_h) = crate::abi::visible_surface_size_for(
        ctx.gpu.width,
        ctx.gpu.phys_width,
        ctx.gpu.height,
        ctx.gpu.phys_height,
    );
    let resize_bottom = visible_h as f32 - 2.0 * crate::layout::LINE_H();
    let resize_y = ((crate::layout::TAB_BAR_H + resize_bottom) * 0.5).max(crate::layout::TAB_BAR_H);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        right + 4.0,
        resize_y,
        0,
    );
    assert_eq!(crate::abi::mui_sidebar_resize_at_click(handle), 1);
    assert_eq!(crate::abi::mui_sidebar_resize_to_event_x(handle), start_w.round() as i32);

    ctx.last_event = MuiEvent::mouse_move(right + 34.0, resize_y, 0);
    assert_eq!(
        crate::abi::mui_sidebar_resize_to_event_x(handle),
        (start_w + 30.0).round() as i32
    );
    assert_eq!(
        crate::abi::mui_sidebar_resize_finish(handle),
        (start_w + 30.0).round() as i32
    );
    assert!(!ctx.sidebar_resizing);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        format!("Sidebar resized to {}px", (start_w + 30.0).round() as i32)
    );
    crate::layout::reset_sidebar_preset();
    crate::layout::set_zen(zen_before);
}

#[test]
fn sidebar_close_command_is_deterministic() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = true;
    ctx.active_panel = crate::PANEL_SEARCH;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::abi::mui_sidebar_close(handle), 1);
    assert!(!ctx.sidebar_visible);
    assert_eq!(ctx.active_panel, crate::PANEL_SEARCH);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Sidebar closed");

    assert_eq!(crate::abi::mui_sidebar_close(handle), 0);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Sidebar is already closed"
    );
}

#[test]
fn visible_rows_reserve_space_for_every_bottom_dock_owner() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::layout::reset_dock_fraction();
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    let base_rows = crate::abi::mui_visible_rows(handle);
    assert!(base_rows > 1);
    assert!(!ctx.bottom_dock_open());

    assert_eq!(crate::featureabi::mui_run_open(handle), 1);
    let run_rows = crate::abi::mui_visible_rows(handle);
    assert!(run_rows < base_rows, "run_rows={run_rows} base_rows={base_rows}");
    assert!(ctx.bottom_dock_open());

    assert_eq!(crate::webabi::mui_web_open(handle), 1);
    let web_rows = crate::abi::mui_visible_rows(handle);
    assert_eq!(web_rows, run_rows, "web dock should reserve the same lower band");
    assert!(ctx.bottom_dock_open());

    assert_eq!(crate::navsurfaces::mui_problems_open(handle), 1);
    let problems_rows = crate::abi::mui_visible_rows(handle);
    assert_eq!(
        problems_rows, run_rows,
        "problems dock should reserve the same lower band"
    );
    assert!(ctx.bottom_dock_open());
    crate::layout::reset_dock_fraction();
}

#[test]
fn run_start_without_file_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.term_open = true;
    ctx.web.open();
    ctx.problems.set_open(true);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_run_start(handle), 0);
    assert!(ctx.run.is_active());
    assert!(!ctx.term_open);
    assert!(!ctx.web.is_active());
    assert!(!ctx.problems.is_open());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No file to run");
}

#[test]
fn run_stop_when_idle_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.term_open = true;
    ctx.web.open();
    ctx.problems.set_open(true);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::featureabi::mui_run_stop(handle);
    assert!(ctx.run.is_active());
    assert!(!ctx.run.is_running());
    assert!(!ctx.term_open);
    assert!(!ctx.web.is_active());
    assert!(!ctx.problems.is_open());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No run process to stop");
}

#[test]
fn run_clear_output_reports_feedback_and_preserves_status() {
    let mut ctx = ctx_or_skip!();
    ctx.term_open = true;
    ctx.web.open();
    ctx.problems.set_open(true);
    ctx.run.seed_demo("C:/proj/demo.mty");
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_run_clear(handle), 8);
    assert!(ctx.run.is_active());
    assert_eq!(ctx.run.line_count(), 0);
    assert_eq!(ctx.run.exit_code(), Some(1));
    assert_eq!(ctx.run.duration_ms(), 142);
    assert!(!ctx.term_open);
    assert!(!ctx.web.is_active());
    assert!(!ctx.problems.is_open());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Run output cleared");

    assert_eq!(crate::featureabi::mui_run_clear(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Run output already empty");
}

#[test]
fn run_output_click_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_run_click_row(h, -1), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No run output row selected");
    assert_eq!(crate::featureabi::mui_run_click_tab(h), -1);

    assert_eq!(crate::featureabi::mui_run_click_row(h, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No run output row selected");

    let missing = std::env::temp_dir().join(format!(
        "mui_run_missing_target_{}.mty",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&missing);
    ctx.run.seed_demo(missing.to_string_lossy().as_ref());

    assert_eq!(crate::featureabi::mui_run_click_row(h, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Run output row has no file target");
    assert_eq!(crate::featureabi::mui_run_click_tab(h), -1);

    assert_eq!(crate::featureabi::mui_run_click_row(h, 2), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(
        toast.message,
        format!(
            "Run target missing: {}",
            missing.file_name().unwrap().to_string_lossy()
        )
    );
    assert_eq!(crate::featureabi::mui_run_click_tab(h), -1);
}

#[test]
fn run_and_diff_row_snapshots_skip_missing_rows() {
    let mut run = crate::run::RunPanel::new();
    assert!(crate::featureabi::run_line_snapshot(&run, 0).is_none());
    run.seed_demo("C:/proj/demo.mty");
    let row = crate::featureabi::run_line_snapshot(&run, 0).expect("demo row");
    assert!(!row.text.is_empty());
    assert!(crate::featureabi::run_line_snapshot(&run, run.line_count()).is_none());

    let mut diff = crate::diff::DiffView::new();
    assert!(crate::featureabi::diff_line_snapshot(&diff, 0).is_none());
    diff.open("src/main.mty", false, "@@ -1 +1 @@\n-old\n+new\n");
    let row = crate::featureabi::diff_line_snapshot(&diff, 0).expect("hunk row");
    assert_eq!(row.kind, crate::diff::LineKind::Hunk);
    assert!(crate::featureabi::diff_line_snapshot(&diff, diff.line_count()).is_none());
}

#[test]
fn test_at_cursor_without_file_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::testabi::mui_test_run_at_cursor(handle), 0);
    assert_eq!(ctx.active_panel, crate::PANEL_TEST);
    assert!(ctx.sidebar_visible);
    assert!(ctx.tests_panel.is_active());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(
        toast.message,
        "Open a Mighty file before running test at cursor"
    );
}

#[test]
fn test_stop_when_idle_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::testabi::mui_test_stop(handle);
    assert_eq!(ctx.active_panel, crate::PANEL_TEST);
    assert!(ctx.sidebar_visible);
    assert!(ctx.tests_panel.is_active());
    assert!(!ctx.tests_panel.is_running());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No test run to stop");
}

#[test]
fn test_clear_results_reports_feedback_and_preserves_context() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    ctx.tests_panel.seed_demo("C:/proj/demo");
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::testabi::mui_test_clear(handle), 8);
    assert_eq!(ctx.active_panel, crate::PANEL_TEST);
    assert!(ctx.sidebar_visible);
    assert!(ctx.tests_panel.is_active());
    assert_eq!(ctx.tests_panel.row_count(), 0);
    assert_eq!(ctx.tests_panel.passed(), 0);
    assert_eq!(ctx.tests_panel.failed(), 0);
    assert_eq!(ctx.tests_panel.total(), 0);
    assert_eq!(ctx.tests_panel.pkg(), "C:/proj/demo");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Test results cleared");

    assert_eq!(crate::testabi::mui_test_clear(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Test results already empty");
}

#[test]
fn test_result_open_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.tests_panel
        .set_click_target(Some(("stale.mty".to_string(), 3, 2)));
    assert_eq!(crate::testabi::mui_test_open_row(handle, -1), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No test result row selected");
    assert_eq!(crate::testabi::mui_test_click_tab(handle), -1);

    ctx.tests_panel.seed_demo("");
    assert_eq!(crate::testabi::mui_test_open_row(handle, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Test result row has no file target");
    assert_eq!(crate::testabi::mui_test_click_tab(handle), -1);

    assert_eq!(crate::testabi::mui_test_open_row(handle, 99), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Test result row has no file target");
    assert_eq!(crate::testabi::mui_test_click_tab(handle), -1);
}

#[test]
fn agents_run_without_file_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::agentsabi::mui_agents_run(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Open a file before running Agents");
}

#[test]
fn agents_clear_run_output_reports_feedback_and_preserves_topology() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    ctx.agents.seed_demo();
    ctx.agents.seed_run_demo("examples/agents.mty");
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::agentsabi::mui_agents_clear_run_output(handle), 8);
    assert_eq!(ctx.active_panel, crate::PANEL_AGENTS_MTY);
    assert!(ctx.sidebar_visible);
    assert_eq!(ctx.agents.run_line_count(), 0);
    assert_eq!(ctx.agents.agent_count(), 2);
    assert_eq!(ctx.agents.protocol_count(), 2);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Agents run output cleared");

    assert_eq!(crate::agentsabi::mui_agents_clear_run_output(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Agents run output already empty");
}

#[test]
fn agents_open_node_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let missing = std::env::temp_dir()
        .join(format!("mui_agents_missing_{}", std::process::id()))
        .join("agent.mty");
    let model = crate::agents::scan_file(&missing, "agent Worker {}\n");
    ctx.agents.set_model(model);
    ctx.agents
        .set_click_target(Some((std::path::PathBuf::from("stale.mty"), 7)));

    assert_eq!(crate::agentsabi::mui_agents_open_node(handle, -1), -1);
    assert!(ctx.agents.click_target().is_none());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No agent node selected");

    assert_eq!(crate::agentsabi::mui_agents_open_node(handle, 0), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Agents node has no file target");

    assert_eq!(crate::agentsabi::mui_agents_open_node(handle, 1), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Agents target missing: agent.mty");
}

#[test]
fn bottom_dock_resize_uses_visible_mouse_geometry() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::layout::reset_dock_fraction();
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 1280;
    ctx.gpu.phys_width = 1280;
    ctx.gpu.height = 832;
    ctx.gpu.phys_height = 832;
    crate::uiscale::set_os_scale(1.375);
    crate::uiscale::set_user_zoom(1.0);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_run_open(handle), 1);
    let visible_w = crate::layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width);
    let visible_h = crate::layout::visible_height(ctx.gpu.height, ctx.gpu.phys_height);
    let default_h = crate::layout::term_panel_height(visible_h).round() as i32;
    let edge_y = crate::layout::term_panel_top(visible_h) + 2.0;
    ctx.last_event = MuiEvent::mouse(MUI_EVENT_MOUSE_DOWN, MUI_MOUSE_LEFT, 500.0, edge_y, 0);
    assert_eq!(crate::abi::mui_bottom_dock_resize_at_click(handle), 1);
    assert_eq!(
        crate::abi::mui_bottom_dock_resize_to_event_y(handle),
        default_h,
        "off-center dock grab should not jump before real pointer movement"
    );

    ctx.last_event = MuiEvent::mouse_move(500.0, edge_y - 40.0, 0);
    let offset_resized_h = crate::abi::mui_bottom_dock_resize_to_event_y(handle);
    assert!(
        offset_resized_h > default_h,
        "moving the captured pointer upward should grow the dock: {offset_resized_h} <= {default_h}"
    );

    ctx.last_event = MuiEvent::mouse_move(500.0, 260.0, 0);
    let resized_h = crate::abi::mui_bottom_dock_resize_to_event_y(handle);
    assert!(resized_h > default_h, "resized_h={resized_h} default_h={default_h}");
    let rows_after_taller = crate::abi::mui_visible_rows(handle);

    ctx.last_event = MuiEvent::mouse_move(500.0, 610.0, 0);
    let shorter_h = crate::abi::mui_bottom_dock_resize_to_event_y(handle);
    assert!(shorter_h < resized_h, "shorter_h={shorter_h} resized_h={resized_h}");
    let rows_after_shorter = crate::abi::mui_visible_rows(handle);
    assert!(rows_after_shorter > rows_after_taller);
    assert_eq!(crate::abi::mui_bottom_dock_resize_finish(handle), shorter_h);
    assert!(!ctx.bottom_dock_resizing);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        format!("Dock resized to {shorter_h}px")
    );

    let (rx, ry, rw, rh) = crate::layout::dock_preset_rect(visible_w, visible_h, 1);
    ctx.last_event = MuiEvent::mouse(
        MUI_EVENT_MOUSE_DOWN,
        MUI_MOUSE_LEFT,
        rx + rw * 0.5,
        ry + rh * 0.5,
        0,
    );
    assert_eq!(crate::abi::mui_bottom_dock_preset_at_click(handle), 2);
    assert_eq!(
        crate::layout::dock_fraction(),
        crate::layout::TERM_FRACTION,
        "default preset should reset the shared dock fraction"
    );
    assert_eq!(crate::layout::dock_preset_index(), 1);
    assert!(!ctx.bottom_dock_resizing);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Dock reset",
        "visible dock preset buttons should confirm the resize"
    );

    assert_eq!(
        crate::abi::mui_dock_dispatch(handle, crate::palette::CMD_DOCK_COMPACT as i32),
        1
    );
    assert_eq!(crate::layout::dock_fraction(), crate::layout::TERM_FRACTION_MIN);
    assert_eq!(crate::layout::dock_preset_index(), 0);
    assert_eq!(
        crate::abi::mui_dock_dispatch(handle, crate::palette::CMD_DOCK_EXPANDED as i32),
        3
    );
    assert_eq!(crate::layout::dock_fraction(), crate::layout::TERM_FRACTION_MAX);
    assert_eq!(crate::layout::dock_preset_index(), 2);
    assert_eq!(
        crate::abi::mui_dock_dispatch(handle, crate::palette::CMD_DOCK_CLOSE as i32),
        4
    );
    assert!(!ctx.bottom_dock_open());
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Bottom dock closed"
    );
    assert_eq!(crate::featureabi::mui_run_open(handle), 1);

    let (cx, cy, cw, ch) = crate::layout::dock_close_rect(visible_w, visible_h);
    ctx.last_event = MuiEvent::mouse(
        MUI_EVENT_MOUSE_DOWN,
        MUI_MOUSE_LEFT,
        cx + cw * 0.5,
        cy + ch * 0.5,
        0,
    );
    assert_eq!(crate::abi::mui_bottom_dock_close_at_click(handle), 1);
    assert!(!ctx.bottom_dock_open());
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Bottom dock closed",
        "visible dock close button should confirm the close"
    );
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    crate::layout::reset_dock_fraction();
}

#[test]
fn terminal_close_acknowledges_state_without_requiring_pty_spawn() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.term_open = true;
    crate::abi::mui_term_close(handle);
    assert!(!ctx.term_open);
    assert!(ctx.terminal.is_none());
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Terminal closed"
    );

    crate::abi::mui_term_close(handle);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Terminal is already closed"
    );
}

#[test]
fn terminal_open_failure_reports_visible_feedback() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_TERM_FORCE_OPEN_FAIL", "1");
    assert_eq!(crate::abi::mui_term_open(handle), 0);
    std::env::remove_var("MUI_TERM_FORCE_OPEN_FAIL");

    assert!(!ctx.term_open);
    assert!(ctx.terminal.is_none());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Error);
    assert_eq!(toast.message, "Terminal failed to open");
}

#[test]
fn account_utility_opens_settings_on_inline_ai_row() {
    use crate::featureabi::{mui_settings_open_account, mui_settings_sel};

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    assert_eq!(mui_settings_open_account(handle), 1);
    assert!(ctx.settings_panel.is_active());
    assert_eq!(
        mui_settings_sel(handle),
        crate::settingspanel::RowId::ALL
            .iter()
            .position(|r| *r == crate::settingspanel::RowId::InlineAi)
            .unwrap() as i32
    );
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
    let sw = crate::layout::sidebar_w();

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

    ctx.search.query = "opened".chars().collect();
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + sw - 26.0, 88.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 0);
    assert!(ctx.search.replace_focus);

    ctx.search.results.matches.push(crate::search::SearchMatch {
        file: 0,
        line: 0,
        col: 0,
        preview: "opened".to_string(),
    });
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + sw - 26.0, 88.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 2);
    assert!(ctx.search.replace_focus);

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, sx + sw - 20.0, 20.0, 0);
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 1);
    assert!(!ctx.search.replace_focus);

    ctx.active_panel = crate::PANEL_EXPLORER;
    assert_eq!(crate::panels::mui_search_action_at_click(handle), 0);
}

#[test]
fn search_replace_all_toasts_visible_result() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_search_replace_toast");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("a.mty");
    std::fs::write(&path, "foo\nfoo\n").unwrap();
    ctx.tree.set_root(root.clone());
    ctx.tabs.open_path(path.clone());
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
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "bar\nbar\n");
    assert_eq!(ctx.tabs.active_model().as_text(), "bar\nbar\n");
    assert!(!ctx.tabs.is_dirty(ctx.tabs.active()));
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Replaced 2 occurrences");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn search_replace_all_preserves_dirty_open_tab() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_search_replace_dirty_tab");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("a.mty");
    std::fs::write(&path, "foo\n").unwrap();
    ctx.tree.set_root(root.clone());
    let idx = ctx.tabs.open_path(path.clone());
    ctx.tabs.active_model_mut().set_text_preserving_cursor("local unsaved foo\n");
    ctx.tabs.set_dirty(idx, true);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    for ch in "foo".chars() {
        ctx.search.push_char(ch as u32);
    }
    assert_eq!(crate::panels::mui_search_run(handle), 1);
    ctx.search.replace_focus = true;
    for ch in "bar".chars() {
        ctx.search.push_char(ch as u32);
    }

    assert_eq!(crate::panels::mui_search_replace_all(handle), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "bar\n");
    assert_eq!(ctx.tabs.active_model().as_text(), "local unsaved foo\n");
    assert!(ctx.tabs.is_dirty(idx));
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(
        toast.message,
        "Replaced 1 occurrence; 1 dirty open tab not refreshed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn search_open_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_search_open(handle, -1), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No search result selected");

    ctx.search.results.matches.push(crate::search::SearchMatch {
        file: 1,
        line: 0,
        col: 0,
        preview: "needle".to_string(),
    });
    assert_eq!(crate::panels::mui_search_open(handle, 0), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Search result file no longer listed");

    let missing = std::env::temp_dir()
        .join(format!("mui_search_missing_{}", std::process::id()))
        .join("hit.mty");
    ctx.search.results.files.push(crate::search::SearchFile {
        path: missing,
        rel: "hit.mty".to_string(),
        match_count: 1,
    });
    ctx.search.results.matches[0].file = 0;
    assert_eq!(crate::panels::mui_search_open(handle, 0), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Search target missing: hit.mty");
}

#[test]
fn new_project_invalid_and_existing_names_toast_without_shelling_out() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_new_project_guards");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("taken")).unwrap();
    std::fs::write(root.join("taken").join("keep.txt"), "existing").unwrap();
    ctx.workspace.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.path_stage.extend_from_slice(b"bad/name");
    assert_eq!(crate::newprojabi::mui_newproj_create(handle), 0);
    assert!(ctx.path_stage.is_empty());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Name must not contain path separators");

    ctx.path_stage.extend_from_slice(b"taken");
    assert_eq!(crate::newprojabi::mui_newproj_create(handle), 0);
    assert!(ctx.path_stage.is_empty());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Choose an empty folder for taken");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_project_dialog_rejects_non_empty_selected_folder() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_new_project_dialog_{}", std::process::id()));
    let target = root.join("chosen_project");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "do not overwrite").unwrap();
    ctx.workspace.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_NEW_PROJECT_PICK", target.to_string_lossy().as_ref());
    assert_eq!(crate::mui_newproj_dialog(handle), 0);
    std::env::remove_var("MUI_NEW_PROJECT_PICK");

    assert!(target.join("keep.txt").exists(), "dialog path must not overwrite non-empty folders");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Choose an empty folder for chosen_project");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_project_dialog_cancel_does_not_open_prompt_fallback() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_NEW_PROJECT_PICK", "");
    assert_eq!(crate::mui_newproj_dialog(handle), 0);
    std::env::remove_var("MUI_NEW_PROJECT_PICK");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "New project cancelled");
}

#[test]
fn new_folder_validates_name_clears_stage_and_toasts() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_new_folder_guards");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("taken")).unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.path_stage.extend_from_slice(b"..\\escape");
    assert_eq!(crate::mui_newfolder_create(handle), 0);
    assert!(ctx.path_stage.is_empty());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Name must not contain path separators");

    ctx.path_stage.extend_from_slice(b"taken");
    assert_eq!(crate::mui_newfolder_create(handle), 0);
    assert!(ctx.path_stage.is_empty());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Folder already exists: taken");

    ctx.path_stage.extend_from_slice(b"fresh");
    assert_eq!(crate::mui_newfolder_create(handle), 1);
    assert!(ctx.path_stage.is_empty());
    assert!(root.join("fresh").is_dir());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Created folder: fresh");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_folder_dialog_env_pick_creates_or_accepts_folder() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_new_folder_dialog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    let fresh = root.join("fresh");
    std::env::set_var("MUI_NEW_FOLDER_PICK", fresh.to_string_lossy().as_ref());
    assert_eq!(crate::mui_newfolder_dialog(handle), 1);
    std::env::remove_var("MUI_NEW_FOLDER_PICK");
    assert!(fresh.is_dir());
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Created folder: fresh"
    );

    let existing = root.join("existing");
    std::fs::create_dir_all(&existing).unwrap();
    std::env::set_var("MUI_NEW_FOLDER_PICK", existing.to_string_lossy().as_ref());
    assert_eq!(crate::mui_newfolder_dialog(handle), 1);
    std::env::remove_var("MUI_NEW_FOLDER_PICK");
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Folder ready: existing"
    );

    let outside = std::env::temp_dir().join(format!(
        "mui_new_folder_dialog_outside_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::env::set_var("MUI_NEW_FOLDER_PICK", outside.to_string_lossy().as_ref());
    assert_eq!(crate::mui_newfolder_dialog(handle), 0);
    std::env::remove_var("MUI_NEW_FOLDER_PICK");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Choose a folder inside the workspace");
    let _ = std::fs::remove_dir_all(&outside);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_folder_dialog_cancel_is_noop() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_NEW_FOLDER_PICK", "");
    assert_eq!(crate::mui_newfolder_dialog(handle), 0);
    std::env::remove_var("MUI_NEW_FOLDER_PICK");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "New folder cancelled");
}

#[test]
fn new_file_validates_name_clears_stage_opens_tab_and_toasts() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_new_file_guards");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("taken.mty"), b"existing").unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.path_stage.extend_from_slice(b"bad/name.mty");
    assert_eq!(crate::mui_newfile_create(handle), -1);
    assert!(ctx.path_stage.is_empty());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Name must not contain path separators");

    ctx.path_stage.extend_from_slice(b"taken.mty");
    assert_eq!(crate::mui_newfile_create(handle), -2);
    assert!(ctx.path_stage.is_empty());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "File already exists: taken.mty");

    ctx.path_stage.extend_from_slice(b"fresh.mty");
    let idx = crate::mui_newfile_create(handle);
    assert!(idx >= 0);
    assert!(ctx.path_stage.is_empty());
    let fresh = root.join("fresh.mty");
    assert!(fresh.is_file());
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "");
    assert_eq!(ctx.tabs.active(), idx as usize);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(fresh.as_path()));
    assert_eq!(ctx.file_path.as_deref(), Some(fresh.as_path()));
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Created file: fresh.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_file_dialog_env_pick_creates_opens_and_records_recent() {
    use crate::{mui_newfile_dialog, mui_quickopen_reindex, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let root = std::env::temp_dir().join(format!("mui_new_file_dialog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    let picked = root.join("picked.mty");

    std::env::set_var("MUI_NEW_FILE_PICK", picked.to_string_lossy().as_ref());
    let idx = mui_newfile_dialog(handle);
    std::env::remove_var("MUI_NEW_FILE_PICK");

    assert_eq!(idx, 1, "dialog-picked new file should open as a new tab");
    assert!(picked.is_file());
    assert_eq!(std::fs::read_to_string(&picked).unwrap(), "");
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), idx);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(picked.as_path()));
    assert_eq!(ctx.file_path.as_deref(), Some(picked.as_path()));
    assert_eq!(mui_quickopen_reindex(handle), 1, "new file should be in the file index");
    assert_eq!(ctx.quickopen.recent_paths(), vec![picked.clone()]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_file_dialog_env_sequence_supports_multiple_dialog_picks() {
    use crate::{mui_newfile_dialog, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let root = std::env::temp_dir().join(format!(
        "mui_new_file_dialog_sequence_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.tree.set_root(root.clone());

    let first = root.join("welcome.mty");
    let second = root.join("explorer.mty");
    let seq = format!("{}|{}", first.display(), second.display());

    std::env::remove_var("MUI_NEW_FILE_PICK");
    std::env::set_var("MUI_NEW_FILE_PICK_SEQUENCE", seq);
    let first_idx = mui_newfile_dialog(handle);
    let second_idx = mui_newfile_dialog(handle);
    std::env::remove_var("MUI_NEW_FILE_PICK_SEQUENCE");

    assert_eq!(first_idx, 1);
    assert_eq!(second_idx, 2);
    assert!(first.is_file());
    assert!(second.is_file());
    assert_eq!(mui_tab_count(handle), 3);
    assert_eq!(mui_tab_active(handle), second_idx);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(second.as_path()));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_file_dialog_cancel_and_existing_are_noops() {
    use crate::{mui_newfile_dialog, mui_newfile_workspace_dialog, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let root = std::env::temp_dir().join(format!("mui_new_file_dialog_noop_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());

    std::env::set_var("MUI_NEW_FILE_PICK", "");
    assert_eq!(mui_newfile_dialog(handle), -2);
    std::env::remove_var("MUI_NEW_FILE_PICK");
    assert_eq!(mui_tab_count(handle), 1);
    assert_eq!(mui_tab_active(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "New file cancelled");

    let existing = root.join("taken.mty");
    std::fs::write(&existing, b"existing").unwrap();
    std::env::set_var("MUI_NEW_FILE_PICK", existing.to_string_lossy().as_ref());
    assert_eq!(mui_newfile_dialog(handle), -2);
    std::env::remove_var("MUI_NEW_FILE_PICK");
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "existing");
    assert_eq!(mui_tab_count(handle), 1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "File already exists: taken.mty");

    let outside_dir = std::env::temp_dir().join(format!(
        "mui_new_file_dialog_outside_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&outside_dir);
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside = outside_dir.join("outside.mty");
    std::env::set_var("MUI_NEW_FILE_PICK", outside.to_string_lossy().as_ref());
    let outside_idx = mui_newfile_dialog(handle);
    std::env::remove_var("MUI_NEW_FILE_PICK");
    assert_eq!(outside_idx, 1);
    assert!(outside.exists());
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), outside_idx);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(outside.as_path()));
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Created file: outside.mty");

    let outside_workspace = outside_dir.join("outside-workspace.mty");
    std::env::set_var("MUI_NEW_FILE_PICK", outside_workspace.to_string_lossy().as_ref());
    assert_eq!(mui_newfile_workspace_dialog(handle), -2);
    std::env::remove_var("MUI_NEW_FILE_PICK");
    assert!(!outside_workspace.exists());
    assert_eq!(mui_tab_count(handle), 2);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Choose a file inside the workspace");

    let _ = std::fs::remove_dir_all(&outside_dir);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_workspace_file_dialog_creates_inside_workspace() {
    use crate::{mui_newfile_workspace_dialog, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let root = std::env::temp_dir().join(format!(
        "mui_new_workspace_file_dialog_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());

    let picked = root.join("inside.mty");
    std::env::set_var("MUI_NEW_FILE_PICK", picked.to_string_lossy().as_ref());
    let idx = mui_newfile_workspace_dialog(handle);
    std::env::remove_var("MUI_NEW_FILE_PICK");

    assert_eq!(idx, 1);
    assert!(picked.exists());
    assert_eq!(mui_tab_count(handle), 2);
    assert_eq!(mui_tab_active(handle), idx);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(picked.as_path()));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn active_file_rename_updates_tab_path_tree_and_toasts() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_active_file_rename");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    let old = root.join("src").join("old.mty");
    let new = root.join("src").join("new.mty");
    std::fs::write(&old, "fn main() {}\n").unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    ctx.tabs.open_path(old.clone());
    crate::abi::sync_active_path(&mut ctx);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.path_stage.extend_from_slice(b"new.mty");
    assert_eq!(crate::mui_file_rename_active(handle), 1);
    assert!(ctx.path_stage.is_empty());
    assert!(!old.exists());
    assert!(new.exists());
    assert_eq!(ctx.tabs.active_path().unwrap(), new);
    assert_eq!(ctx.file_path.as_ref().unwrap().file_name().unwrap(), "new.mty");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Renamed to new.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn active_file_delete_requires_exact_basename_confirmation() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_active_file_delete");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let keep = root.join("keep.mty");
    let doomed = root.join("doomed.mty");
    std::fs::write(&keep, "fn keep() {}\n").unwrap();
    std::fs::write(&doomed, "fn doomed() {}\n").unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    ctx.tabs.open_path(keep.clone());
    ctx.tabs.open_path(doomed.clone());
    crate::abi::sync_active_path(&mut ctx);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.path_stage.extend_from_slice(b"wrong.mty");
    assert_eq!(crate::mui_file_delete_active_confirm(handle), 0);
    assert!(ctx.path_stage.is_empty());
    assert!(doomed.exists());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Type doomed.mty to delete");

    ctx.path_stage.extend_from_slice(b"doomed.mty");
    assert_eq!(crate::mui_file_delete_active_confirm(handle), 1);
    assert!(!doomed.exists());
    assert_eq!(ctx.tabs.count(), 2);
    assert_eq!(ctx.tabs.active_path().unwrap(), keep);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Deleted doomed.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn delete_prompt_label_names_exact_file_before_confirmation() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_delete_prompt_label");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("doomed.mty");
    std::fs::write(&file, "fn doomed() {}\n").unwrap();
    ctx.tabs.open_path(file);
    crate::abi::sync_active_path(&mut ctx);
    ctx.prompt.open(crate::prompt::PromptKind::DeleteFile as i32);

    assert_eq!(
        crate::abi::prompt_draw_label(&ctx),
        "Delete doomed.mty, type name: "
    );

    ctx.prompt.open(crate::prompt::PromptKind::RenameFile as i32);
    assert_eq!(
        crate::abi::prompt_draw_label(&ctx),
        "Rename active file to: "
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn active_file_reveal_commands_are_named_for_their_scope() {
    let new_file = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_NEW_FILE)
        .unwrap();
    assert_eq!(new_file.label, "File: New File...");
    assert_eq!(new_file.keybinding, "Ctrl+N");

    let tree = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_REVEAL_ACTIVE_FILE)
        .unwrap();
    assert_eq!(tree.label, "File: Reveal Active File in File Tree");

    let refresh_tree = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_EXPLORER_REFRESH)
        .unwrap();
    assert_eq!(refresh_tree.label, "Explorer: Refresh");
    assert_eq!(refresh_tree.keybinding, "");

    let collapse_tree = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_EXPLORER_COLLAPSE_ALL)
        .unwrap();
    assert_eq!(collapse_tree.label, "Explorer: Collapse All Folders");

    let os = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_REVEAL_ACTIVE_FILE_IN_OS)
        .unwrap();
    assert_eq!(os.label, "File: Show Active File in File Manager");

    let copy_path = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_COPY_ACTIVE_FILE_PATH)
        .unwrap();
    assert_eq!(copy_path.label, "File: Copy Active File Path");

    let copy_relative = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_COPY_ACTIVE_FILE_RELATIVE_PATH)
        .unwrap();
    assert_eq!(copy_relative.label, "File: Copy Active File Relative Path");

    let copy_name = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_COPY_ACTIVE_FILE_NAME)
        .unwrap();
    assert_eq!(copy_name.label, "File: Copy Active File Name");

    let copy_directory = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_COPY_ACTIVE_FILE_DIRECTORY)
        .unwrap();
    assert_eq!(copy_directory.label, "File: Copy Active File Directory");

    let clear_notifications = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_CLEAR_NOTIFICATIONS)
        .unwrap();
    assert_eq!(clear_notifications.label, "Notifications: Clear All Toasts");

    let save_all = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_SAVE_ALL)
        .unwrap();
    assert_eq!(save_all.label, "File: Save All");

    let close_saved = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_CLOSE_SAVED_TABS)
        .unwrap();
    assert_eq!(close_saved.label, "File: Close Saved Tabs");

    let close_other_saved = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_CLOSE_OTHER_SAVED_TABS)
        .unwrap();
    assert_eq!(close_other_saved.label, "File: Close Other Saved Tabs");

    let close_right = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_CLOSE_SAVED_TABS_TO_RIGHT)
        .unwrap();
    assert_eq!(close_right.label, "File: Close Saved Tabs to the Right");

    let close_left = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_CLOSE_SAVED_TABS_TO_LEFT)
        .unwrap();
    assert_eq!(close_left.label, "File: Close Saved Tabs to the Left");

    let reopen_closed = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_REOPEN_CLOSED_TAB)
        .unwrap();
    assert_eq!(reopen_closed.label, "File: Reopen Closed Tab");
    assert_eq!(reopen_closed.keybinding, "Ctrl+Alt+T");

    let duplicate_tab = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_DUPLICATE_ACTIVE_TAB)
        .unwrap();
    assert_eq!(duplicate_tab.label, "File: Duplicate Active Tab");

    let move_left = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_MOVE_ACTIVE_TAB_LEFT)
        .unwrap();
    assert_eq!(move_left.label, "File: Move Active Tab Left");
    assert_eq!(move_left.keybinding, "Ctrl+Shift+PageUp");

    let move_right = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_MOVE_ACTIVE_TAB_RIGHT)
        .unwrap();
    assert_eq!(move_right.label, "File: Move Active Tab Right");
    assert_eq!(move_right.keybinding, "Ctrl+Shift+PageDown");

    let sort_tabs = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_SORT_TABS_BY_NAME)
        .unwrap();
    assert_eq!(sort_tabs.label, "File: Sort Open Tabs by Name");

    let close_duplicates = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_CLOSE_DUPLICATE_TABS)
        .unwrap();
    assert_eq!(close_duplicates.label, "File: Close Duplicate Tabs");

    let reload_file = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_RELOAD_ACTIVE_FILE)
        .unwrap();
    assert_eq!(reload_file.label, "File: Reload Active File from Disk");

    let revert_file = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_REVERT_ACTIVE_FILE)
        .unwrap();
    assert_eq!(revert_file.label, "File: Revert Active File from Disk");

    let stage_all = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_GIT_STAGE_ALL)
        .unwrap();
    assert_eq!(stage_all.label, "Git: Stage All");

    let unstage_all = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_GIT_UNSTAGE_ALL)
        .unwrap();
    assert_eq!(unstage_all.label, "Git: Unstage All");

    let commit_staged = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_GIT_COMMIT_STAGED)
        .unwrap();
    assert_eq!(commit_staged.label, "Git: Commit Staged");

    let refresh_scm = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_GIT_REFRESH_SOURCE_CONTROL)
        .unwrap();
    assert_eq!(refresh_scm.label, "Git: Refresh Source Control");
    assert_eq!(refresh_scm.keybinding, "");

    let search_commands = [
        (crate::palette::CMD_SEARCH_RUN, "Search: Run Search", ""),
        (
            crate::palette::CMD_SEARCH_REPLACE_ALL,
            "Search: Replace All",
            "",
        ),
        (
            crate::palette::CMD_SEARCH_TOGGLE_REPLACE,
            "Search: Toggle Replace Field",
            "",
        ),
    ];
    for (id, label, keybinding) in search_commands {
        let cmd = crate::palette::COMMANDS.iter().find(|cmd| cmd.id == id).unwrap();
        assert_eq!(cmd.label, label);
        assert_eq!(cmd.keybinding, keybinding);
    }

    let view_commands = [
        (crate::palette::CMD_VIEW_EXPLORER, "View: Explorer"),
        (crate::palette::CMD_VIEW_SEARCH, "View: Search"),
        (crate::palette::CMD_VIEW_SOURCE_CONTROL, "View: Source Control"),
        (crate::palette::CMD_VIEW_OUTLINE, "View: Outline"),
        (crate::palette::CMD_VIEW_RUN_DEBUG, "View: Run and Debug"),
        (crate::palette::CMD_VIEW_TESTING, "View: Testing"),
        (crate::palette::CMD_VIEW_RUN_OUTPUT, "View: Run Output"),
        (crate::palette::CMD_VIEW_PROBLEMS, "View: Problems"),
        (crate::palette::CMD_VIEW_AI_COPILOT, "View: AI Copilot"),
        (crate::palette::CMD_AI_CLOSE, "View: Close AI Copilot"),
        (crate::palette::CMD_SIDEBAR_CLOSE, "View: Close Sidebar"),
        (crate::palette::CMD_VIEW_TERMINAL, "View: Terminal"),
        (crate::palette::CMD_VIEW_WEB_PLAYGROUND, "View: Web Playground"),
        (crate::palette::CMD_DOCK_COMPACT, "View: Bottom Dock Compact"),
        (
            crate::palette::CMD_DOCK_RESET,
            "View: Bottom Dock Default Size",
        ),
        (
            crate::palette::CMD_DOCK_EXPANDED,
            "View: Bottom Dock Expanded",
        ),
        (crate::palette::CMD_DOCK_CLOSE, "View: Close Bottom Dock"),
        (crate::palette::CMD_SIDEBAR_COMPACT, "View: Sidebar Compact"),
        (
            crate::palette::CMD_SIDEBAR_DEFAULT,
            "View: Sidebar Default Width",
        ),
        (crate::palette::CMD_SIDEBAR_WIDE, "View: Sidebar Wide"),
        (
            crate::palette::CMD_WINDOW_TOGGLE_MAXIMIZE,
            "Window: Toggle Maximize",
        ),
        (crate::palette::CMD_WINDOW_MINIMIZE, "Window: Minimize"),
    ];
    for (id, label) in view_commands {
        let cmd = crate::palette::COMMANDS.iter().find(|cmd| cmd.id == id).unwrap();
        assert_eq!(cmd.label, label);
    }

    let problems_refresh = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_PROBLEMS_REFRESH)
        .unwrap();
    assert_eq!(problems_refresh.label, "Problems: Refresh Diagnostics");
    assert_eq!(problems_refresh.keybinding, "");

    let outline_refresh = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_OUTLINE_REFRESH)
        .unwrap();
    assert_eq!(outline_refresh.label, "Outline: Refresh Symbols");
    assert_eq!(outline_refresh.keybinding, "");

    let ai_commands = [
        (crate::palette::CMD_INLINE_AI_ASK, "AI: Inline Ask", "Ctrl+I"),
        (
            crate::palette::CMD_FORCE_GHOST_COMPLETION,
            "AI: Force Ghost Completion",
            "Alt+\\",
        ),
        (crate::palette::CMD_AI_CLEAR_CHAT, "AI: Clear Chat", ""),
        (
            crate::palette::CMD_AGENTS_REFRESH,
            "Mighty Agents: Refresh Topology",
            "",
        ),
    ];
    for (id, label, keybinding) in ai_commands {
        let cmd = crate::palette::COMMANDS.iter().find(|cmd| cmd.id == id).unwrap();
        assert_eq!(cmd.label, label);
        assert_eq!(cmd.keybinding, keybinding);
    }

    let debug_commands = [
        (
            crate::palette::CMD_DEBUG_START_CONTINUE,
            "Debug: Start / Continue",
            "F5",
        ),
        (crate::palette::CMD_DEBUG_STOP, "Debug: Stop", "Shift+F5"),
        (crate::palette::CMD_DEBUG_STEP_OVER, "Debug: Step Over", "F10"),
        (crate::palette::CMD_DEBUG_STEP_INTO, "Debug: Step Into", "F11"),
        (
            crate::palette::CMD_DEBUG_STEP_OUT,
            "Debug: Step Out",
            "Shift+F11",
        ),
        (crate::palette::CMD_DEBUG_PAUSE, "Debug: Pause", ""),
        (crate::palette::CMD_DEBUG_RESTART, "Debug: Restart", ""),
    ];
    for (id, label, keybinding) in debug_commands {
        let cmd = crate::palette::COMMANDS.iter().find(|cmd| cmd.id == id).unwrap();
        assert_eq!(cmd.label, label);
        assert_eq!(cmd.keybinding, keybinding);
    }

    let jump_back = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_JUMP_BACK)
        .unwrap();
    assert_eq!(jump_back.label, "Jump Back");
    assert_eq!(jump_back.keybinding, "");

    let zoom_commands = [
        (crate::palette::CMD_ZOOM_IN, "View: Zoom In", "Ctrl+="),
        (crate::palette::CMD_ZOOM_OUT, "View: Zoom Out", "Ctrl+-"),
        (crate::palette::CMD_ZOOM_RESET, "View: Reset Zoom", "Ctrl+0"),
    ];
    for (id, label, keybinding) in zoom_commands {
        let cmd = crate::palette::COMMANDS.iter().find(|cmd| cmd.id == id).unwrap();
        assert_eq!(cmd.label, label);
        assert_eq!(cmd.keybinding, keybinding);
    }

    let run_stop = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_RUN_STOP)
        .unwrap();
    assert_eq!(run_stop.label, "Run: Stop Process");
    assert_eq!(run_stop.keybinding, "");

    let run_clear = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_RUN_CLEAR_OUTPUT)
        .unwrap();
    assert_eq!(run_clear.label, "Run: Clear Output");
    assert_eq!(run_clear.keybinding, "");

    let test_stop = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_TEST_STOP)
        .unwrap();
    assert_eq!(test_stop.label, "Test: Stop Run");
    assert_eq!(test_stop.keybinding, "");

    let test_clear = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_TEST_CLEAR_RESULTS)
        .unwrap();
    assert_eq!(test_clear.label, "Test: Clear Results");
    assert_eq!(test_clear.keybinding, "");

    let test_at_cursor = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_RUN_TEST_AT_CURSOR)
        .unwrap();
    assert_eq!(test_at_cursor.label, "Run Test at Cursor");
    assert_eq!(test_at_cursor.keybinding, "");

    let peek_close = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_PEEK_CLOSE)
        .unwrap();
    assert_eq!(peek_close.label, "Peek: Close View");
    assert_eq!(peek_close.keybinding, "");

    let web_stop = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_WEB_STOP)
        .unwrap();
    assert_eq!(web_stop.label, "Web: Stop Server");
    assert_eq!(web_stop.keybinding, "");

    let web_open = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_WEB_OPEN_BROWSER)
        .unwrap();
    assert_eq!(web_open.label, "Web: Open in Browser");
    assert_eq!(web_open.keybinding, "");

    let web_clear = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_WEB_CLEAR_OUTPUT)
        .unwrap();
    assert_eq!(web_clear.label, "Web: Clear Output");
    assert_eq!(web_clear.keybinding, "");

    let diff_close = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_DIFF_CLOSE_VIEW)
        .unwrap();
    assert_eq!(diff_close.label, "Diff: Close View");
    assert_eq!(diff_close.keybinding, "");

    let blame_hide = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_GIT_HIDE_BLAME)
        .unwrap();
    assert_eq!(blame_hide.label, "Git: Hide Blame");
    assert_eq!(blame_hide.keybinding, "");

    let agents_clear = crate::palette::COMMANDS
        .iter()
        .find(|cmd| cmd.id == crate::palette::CMD_AGENTS_CLEAR_RUN_OUTPUT)
        .unwrap();
    assert_eq!(agents_clear.label, "Mighty Agents: Clear Run Output");
    assert_eq!(agents_clear.keybinding, "");
}

#[test]
fn save_all_prompts_for_dirty_untitled_tabs() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_save_all_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = root.join("a.mty");
    let b = root.join("b.mty");
    let u = root.join("untitled_saved.mty");
    std::fs::write(&a, "old a").unwrap();
    std::fs::write(&b, "old b").unwrap();

    let ia = ctx.tabs.open_path(a.clone());
    ctx.tabs.active_model_mut().set_text_preserving_cursor("new a");
    ctx.tabs.set_dirty(ia, true);
    let ib = ctx.tabs.open_path(b.clone());
    ctx.tabs.active_model_mut().set_text_preserving_cursor("new b");
    ctx.tabs.set_dirty(ib, true);
    let iu = ctx.tabs.new_untitled();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("untitled");
    ctx.tabs.set_dirty(iu, true);

    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    std::env::set_var("MUI_SAVE_FILE_PICK_SEQUENCE", u.to_string_lossy().as_ref());
    assert_eq!(crate::mui_save_all(handle), 3);
    std::env::remove_var("MUI_SAVE_FILE_PICK_SEQUENCE");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "new a\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "new b\n");
    assert_eq!(std::fs::read_to_string(&u).unwrap(), "untitled\n");
    assert!(!ctx.tabs.is_dirty(ia));
    assert!(!ctx.tabs.is_dirty(ib));
    assert!(!ctx.tabs.is_dirty(iu));
    assert_eq!(ctx.tabs.get(iu).unwrap().path.as_deref(), Some(u.as_path()));
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Saved 3 files");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn save_all_cancelled_untitled_picker_preserves_dirty_tab() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let iu = ctx.tabs.new_untitled();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("untitled");
    ctx.tabs.set_dirty(iu, true);

    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    std::env::set_var("MUI_SAVE_FILE_PICK", "");
    assert_eq!(crate::mui_save_all(handle), 0);
    std::env::remove_var("MUI_SAVE_FILE_PICK");
    assert!(ctx.tabs.is_dirty(iu));
    assert!(ctx.tabs.get(iu).unwrap().path.is_none());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Save All cancelled; 1 untitled file still unsaved");
}

#[test]
fn scm_stage_all_and_unstage_all_via_abi_or_skip() {
    use std::process::Command;
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("scm_stage_all_and_unstage_all_via_abi_or_skip: git not found - skipping");
        return;
    }
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_scm_abi_stage_all_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        Command::new("git").arg("-C").arg(&root).args(args).output().unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    let _ = git(&["config", "user.email", "t@e.st"]);
    let _ = git(&["config", "user.name", "Test"]);
    std::fs::write(root.join("tracked.mty"), "old\n").unwrap();
    assert!(git(&["add", "tracked.mty"]).status.success());
    assert!(git(&["commit", "-q", "-m", "init"]).status.success());
    std::fs::write(root.join("tracked.mty"), "new\n").unwrap();
    std::fs::write(root.join("fresh.mty"), "fresh\n").unwrap();
    ctx.scm.root = Some(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_scm_refresh(handle), 2);
    assert_eq!(ctx.scm.status.staged_count(), 0);
    assert_eq!(crate::panels::mui_scm_stage_all(handle), 1);
    assert_eq!(ctx.scm.status.staged_count(), 2);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Staged all changes");
    assert_eq!(crate::panels::mui_scm_unstage_all(handle), 1);
    assert_eq!(ctx.scm.status.staged_count(), 0);
    assert_eq!(ctx.scm.status.unstaged_count(), 2);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Unstaged all changes");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scm_toggle_stage_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_scm_toggle_stage(handle, -1), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No source control row selected");

    assert_eq!(crate::panels::mui_scm_toggle_stage(handle, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No source control row selected");

    ctx.scm.status.entries.push(crate::scm::ScmEntry {
        path: "tracked.mty".to_string(),
        staged: false,
        status: 'M',
    });
    assert_eq!(crate::panels::mui_scm_toggle_stage(handle, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Source control root missing");
}

#[test]
fn scm_bulk_actions_without_repo_report_not_git_repository() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_scm_no_repo_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_scm_stage_all(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Not a git repository");

    assert_eq!(crate::panels::mui_scm_unstage_all(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Not a git repository");

    assert_eq!(crate::panels::mui_scm_commit(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Not a git repository");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scm_header_refresh_icon_maps_to_refresh_action() {
    use crate::ffi::MuiEvent;

    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(900);
    ctx.sidebar_visible = true;
    ctx.active_panel = crate::PANEL_SCM;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        sx + sw - 28.0 + 7.0,
        20.0,
        0,
    );
    assert_eq!(crate::panels::mui_scm_header_action_at_click(handle), 4);

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        sx + sw - 50.0 + 7.0,
        20.0,
        0,
    );
    assert_eq!(crate::panels::mui_scm_header_action_at_click(handle), 3);
    crate::layout::reset_sidebar_preset();
}

#[test]
fn scm_commit_staged_uses_message_buffer_via_abi_or_skip() {
    use std::process::Command;
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("scm_commit_staged_uses_message_buffer_via_abi_or_skip: git not found - skipping");
        return;
    }
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_scm_commit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        Command::new("git").arg("-C").arg(&root).args(args).output().unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    let _ = git(&["config", "user.email", "t@e.st"]);
    let _ = git(&["config", "user.name", "Test"]);
    std::fs::write(root.join("first.mty"), "first\n").unwrap();
    assert!(git(&["add", "first.mty"]).status.success());
    assert!(git(&["commit", "-q", "-m", "init"]).status.success());
    std::fs::write(root.join("second.mty"), "second\n").unwrap();
    ctx.scm.root = Some(root.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_scm_stage_all(handle), 1);
    for ch in "add second".chars() {
        crate::panels::mui_scm_msg_push(handle, ch as i32);
    }
    assert_eq!(crate::panels::mui_scm_commit(handle), 1);
    assert_eq!(ctx.scm.message_string(), "");
    assert_eq!(ctx.scm.count(), 0);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Committed changes");
    let log = String::from_utf8_lossy(&git(&["log", "-1", "--pretty=%s"]).stdout)
        .trim()
        .to_string();
    assert_eq!(log, "add second");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn diff_open_noops_report_visible_feedback() {
    use crate::scm::ScmEntry;

    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_diff_open(handle, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No file to diff");

    assert_eq!(crate::featureabi::mui_diff_open_row(handle, -1), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No source-control row");

    ctx.scm.status.entries.push(ScmEntry {
        path: "tracked.mty".to_string(),
        staged: false,
        status: 'M',
    });
    assert_eq!(crate::featureabi::mui_diff_open_row(handle, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No git repository for diff");
}

#[test]
fn diff_close_clears_inline_view() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    assert_eq!(
        ctx.diff.open("src/main.mty", false, "@@ -1 +1 @@\n-old\n+new\n"),
        3
    );
    assert!(ctx.diff.is_active());

    crate::featureabi::mui_diff_close(handle);

    assert!(!ctx.diff.is_active());
    assert_eq!(ctx.diff.line_count(), 0);
}

#[test]
fn diff_open_empty_blob_reports_clean_file_or_skip() {
    use crate::scm::ScmEntry;
    use std::process::Command;

    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("diff_open_empty_blob_reports_clean_file_or_skip: git not found - skipping");
        return;
    }

    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_diff_noop_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        Command::new("git").arg("-C").arg(&root).args(args).output().unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    let _ = git(&["config", "user.email", "t@e.st"]);
    let _ = git(&["config", "user.name", "Test"]);
    std::fs::write(root.join("clean.mty"), "clean\n").unwrap();
    assert!(git(&["add", "clean.mty"]).status.success());
    assert!(git(&["commit", "-q", "-m", "init"]).status.success());

    ctx.scm.root = Some(root.clone());
    ctx.scm.status.entries.push(ScmEntry {
        path: "clean.mty".to_string(),
        staged: false,
        status: 'M',
    });
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::featureabi::mui_diff_open_row(handle, 0), 0);
    assert!(!ctx.diff.is_active());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No diff for clean.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn close_saved_tabs_preserves_dirty_buffers_and_reports_count() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_close_saved_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let clean_a = root.join("clean_a.mty");
    let dirty_b = root.join("dirty_b.mty");
    let clean_c = root.join("clean_c.mty");
    std::fs::write(&clean_a, "a").unwrap();
    std::fs::write(&dirty_b, "b").unwrap();
    std::fs::write(&clean_c, "c").unwrap();

    ctx.tabs.open_path(clean_a);
    let dirty = ctx.tabs.open_path(dirty_b);
    ctx.tabs.set_dirty(dirty, true);
    ctx.tabs.open_path(clean_c);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_close_saved(handle), 0);
    assert_eq!(ctx.tabs.count(), 1);
    assert!(ctx.tabs.is_dirty(0));
    assert_eq!(ctx.tabs.get(0).unwrap().basename(), "dirty_b.mty");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Closed 3 saved tabs");

    assert_eq!(crate::mui_tab_reopen_closed(handle), 1);
    assert_eq!(ctx.tabs.active(), 1);
    assert_eq!(ctx.tabs.get(1).unwrap().basename(), "clean_c.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reopened clean_c.mty");

    assert_eq!(crate::mui_tab_reopen_closed(handle), 2);
    assert_eq!(ctx.tabs.active(), 2);
    assert_eq!(ctx.tabs.get(2).unwrap().basename(), "clean_a.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reopened clean_a.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn close_other_saved_tabs_keeps_active_and_dirty_buffers() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_close_other_saved_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let active_a = root.join("active_a.mty");
    let dirty_b = root.join("dirty_b.mty");
    let clean_c = root.join("clean_c.mty");
    std::fs::write(&active_a, "a").unwrap();
    std::fs::write(&dirty_b, "b").unwrap();
    std::fs::write(&clean_c, "c").unwrap();

    let active = ctx.tabs.open_path(active_a);
    let dirty = ctx.tabs.open_path(dirty_b);
    ctx.tabs.set_dirty(dirty, true);
    ctx.tabs.open_path(clean_c);
    ctx.tabs.switch(active);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_close_other_saved(handle), 0);
    assert_eq!(ctx.tabs.count(), 2);
    assert_eq!(ctx.tabs.active(), 0);
    assert_eq!(ctx.tabs.get(0).unwrap().basename(), "active_a.mty");
    assert_eq!(ctx.tabs.get(1).unwrap().basename(), "dirty_b.mty");
    assert!(ctx.tabs.is_dirty(1));
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Closed 2 other saved tabs");

    assert_eq!(crate::mui_tab_reopen_closed(handle), 2);
    assert_eq!(ctx.tabs.active(), 2);
    assert_eq!(ctx.tabs.get(2).unwrap().basename(), "clean_c.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reopened clean_c.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn close_saved_tabs_to_side_preserves_dirty_buffers() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_close_saved_side_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let dirty_left = root.join("dirty_left.mty");
    let clean_left = root.join("clean_left.mty");
    let active_mid = root.join("active_mid.mty");
    let clean_right = root.join("clean_right.mty");
    let dirty_right = root.join("dirty_right.mty");
    for path in [&dirty_left, &clean_left, &active_mid, &clean_right, &dirty_right] {
        std::fs::write(path, "x").unwrap();
    }

    let left_dirty_idx = ctx.tabs.open_path(dirty_left);
    ctx.tabs.set_dirty(left_dirty_idx, true);
    ctx.tabs.open_path(clean_left);
    let active_idx = ctx.tabs.open_path(active_mid);
    ctx.tabs.open_path(clean_right);
    let right_dirty_idx = ctx.tabs.open_path(dirty_right);
    ctx.tabs.set_dirty(right_dirty_idx, true);
    ctx.tabs.switch(active_idx);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_close_saved_to_right(handle), 3);
    assert_eq!(ctx.tabs.count(), 5);
    assert_eq!(ctx.tabs.get(0).unwrap().basename(), "(scratch)");
    assert_eq!(ctx.tabs.get(1).unwrap().basename(), "dirty_left.mty");
    assert_eq!(ctx.tabs.get(2).unwrap().basename(), "clean_left.mty");
    assert_eq!(ctx.tabs.get(3).unwrap().basename(), "active_mid.mty");
    assert_eq!(ctx.tabs.get(4).unwrap().basename(), "dirty_right.mty");
    let right_toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(right_toast.message, "Closed 1 saved tab to the right");

    assert_eq!(crate::mui_tab_close_saved_to_left(handle), 1);
    assert_eq!(ctx.tabs.count(), 3);
    assert_eq!(ctx.tabs.active(), 1);
    assert_eq!(ctx.tabs.get(0).unwrap().basename(), "dirty_left.mty");
    assert_eq!(ctx.tabs.get(1).unwrap().basename(), "active_mid.mty");
    assert_eq!(ctx.tabs.get(2).unwrap().basename(), "dirty_right.mty");
    assert!(ctx.tabs.is_dirty(0));
    assert!(ctx.tabs.is_dirty(2));
    let left_toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(left_toast.message, "Closed 2 saved tabs to the left");

    assert_eq!(crate::mui_tab_reopen_closed(handle), 3);
    assert_eq!(ctx.tabs.active(), 3);
    assert_eq!(ctx.tabs.get(3).unwrap().basename(), "clean_left.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reopened clean_left.mty");

    assert_eq!(crate::mui_tab_reopen_closed(handle), 4);
    assert_eq!(ctx.tabs.active(), 4);
    assert_eq!(ctx.tabs.get(4).unwrap().basename(), "clean_right.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reopened clean_right.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reopen_closed_tab_restores_last_closed_tab_and_toasts() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_reopen_closed_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = root.join("a.mty");
    let b = root.join("b.mty");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    ctx.tabs.open_path(a);
    let b_idx = ctx.tabs.open_path(b);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_close(handle, b_idx as i32), 1);
    assert_eq!(ctx.tabs.count(), 2);
    assert_eq!(crate::mui_tab_reopen_closed(handle), 2);
    assert_eq!(ctx.tabs.active(), 2);
    assert_eq!(ctx.tabs.get(2).unwrap().basename(), "b.mty");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Reopened b.mty");

    assert_eq!(crate::mui_tab_reopen_closed(handle), -1);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "No closed tab to reopen");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn duplicate_active_tab_clones_live_state_and_toasts() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_duplicate_tab_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = root.join("a.mty");
    let b = root.join("b.mty");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    ctx.tabs.open_path(a);
    let b_idx = ctx.tabs.open_path(b);
    ctx.tabs.active_model_mut().set_text_preserving_cursor("dirty b");
    ctx.tabs.store_commit(b_idx, 4, 3, 2);
    ctx.tabs.set_dirty(b_idx, true);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_duplicate_active(handle), 3);
    assert_eq!(ctx.tabs.count(), 4);
    assert_eq!(ctx.tabs.active(), 3);
    assert_eq!(ctx.tabs.get(3).unwrap().basename(), "b.mty");
    assert!(ctx.tabs.is_dirty(3));
    assert_eq!(String::from_utf8(ctx.tabs.active_model().to_bytes()).unwrap(), "dirty b");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Duplicated b.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn move_active_tab_left_right_preserves_split_pane_documents() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_move_tab_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = root.join("a.mty");
    let b = root.join("b.mty");
    let c = root.join("c.mty");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    std::fs::write(&c, "c").unwrap();
    let a_idx = ctx.tabs.open_path(a);
    let b_idx = ctx.tabs.open_path(b);
    ctx.tabs.open_path(c);
    ctx.tabs.switch(b_idx);
    ctx.panes = crate::panes::PaneLayout::new(a_idx);
    ctx.panes.split_right(b_idx, 0);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    let moved_left = b_idx - 1;
    assert_eq!(crate::mui_tab_move_active_left(handle), moved_left as i32);
    assert_eq!(ctx.tabs.active(), moved_left);
    assert_eq!(ctx.tabs.get(moved_left).unwrap().basename(), "b.mty");
    assert_eq!(ctx.panes.tab_at(0), Some(b_idx), "left pane should still show a.mty");
    assert_eq!(ctx.panes.tab_at(1), Some(moved_left), "right pane should follow b.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Moved tab left");

    while ctx.tabs.active() > 0 {
        let before = ctx.tabs.active();
        assert_eq!(crate::mui_tab_move_active_left(handle), (before - 1) as i32);
        assert_eq!(ctx.tabs.get(ctx.tabs.active()).unwrap().basename(), "b.mty");
        assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Moved tab left");
    }

    assert_eq!(crate::mui_tab_move_active_left(handle), -1);
    assert_eq!(ctx.tabs.active(), 0);
    assert_eq!(ctx.tabs.get(0).unwrap().basename(), "b.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().kind, crate::toast::Kind::Info);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Tab is already first");

    while ctx.tabs.active() < b_idx {
        let before = ctx.tabs.active();
        assert_eq!(crate::mui_tab_move_active_right(handle), (before + 1) as i32);
        assert_eq!(ctx.tabs.get(ctx.tabs.active()).unwrap().basename(), "b.mty");
        assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Moved tab right");
    }

    assert_eq!(ctx.tabs.active(), b_idx);
    assert_eq!(ctx.tabs.get(b_idx).unwrap().basename(), "b.mty");
    assert_eq!(ctx.panes.tab_at(0), Some(a_idx), "left pane should follow a.mty back");
    assert_eq!(ctx.panes.tab_at(1), Some(b_idx), "right pane should follow b.mty back");

    while ctx.tabs.active() + 1 < ctx.tabs.count() {
        let before = ctx.tabs.active();
        assert_eq!(crate::mui_tab_move_active_right(handle), (before + 1) as i32);
        assert_eq!(ctx.tabs.get(ctx.tabs.active()).unwrap().basename(), "b.mty");
        assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Moved tab right");
    }

    let last_idx = ctx.tabs.count() - 1;
    assert_eq!(ctx.tabs.active(), last_idx);
    assert_eq!(ctx.tabs.get(last_idx).unwrap().basename(), "b.mty");
    assert_eq!(ctx.panes.tab_at(0), Some(a_idx), "left pane should keep a.mty");
    assert_eq!(ctx.panes.tab_at(1), Some(last_idx), "right pane should follow b.mty to the edge");

    assert_eq!(crate::mui_tab_move_active_right(handle), -1);
    assert_eq!(ctx.tabs.active(), last_idx);
    assert_eq!(ctx.tabs.get(last_idx).unwrap().basename(), "b.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().kind, crate::toast::Kind::Info);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Tab is already last");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sort_tabs_by_name_preserves_active_and_split_pane_documents() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_sort_tabs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let z = root.join("zeta.mty");
    let a = root.join("alpha.mty");
    let m = root.join("middle.mty");
    std::fs::write(&z, "z").unwrap();
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&m, "m").unwrap();
    let z_idx = ctx.tabs.open_path(z);
    let a_idx = ctx.tabs.open_path(a);
    ctx.tabs.open_path(m);
    ctx.tabs.switch(a_idx);
    ctx.panes = crate::panes::PaneLayout::new(z_idx);
    ctx.panes.split_right(a_idx, 0);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_sort_by_name(handle), 1);
    assert_eq!(ctx.tabs.active(), 1);
    assert_eq!(ctx.tabs.get(1).unwrap().basename(), "alpha.mty");
    assert_eq!(ctx.panes.tab_at(0), Some(3), "left pane should still show zeta.mty");
    assert_eq!(ctx.panes.tab_at(1), Some(1), "right pane should still show alpha.mty");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Sorted tabs by name");

    assert_eq!(crate::mui_tab_sort_by_name(handle), -1);
    assert_eq!(ctx.toasts.toasts().last().unwrap().kind, crate::toast::Kind::Info);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Tabs already sorted");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn close_duplicate_tabs_preserves_active_dirty_and_valid_panes() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_close_duplicate_tabs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = root.join("a.mty");
    let b = root.join("b.mty");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    ctx.tabs.open_path(a.clone());
    let b_idx = ctx.tabs.open_path(b);
    let duplicate_b = ctx.tabs.duplicate_active();
    let dirty_duplicate_b = ctx.tabs.duplicate_active();
    ctx.tabs.set_dirty(dirty_duplicate_b, true);
    ctx.tabs.open_path(a);
    let duplicate_a = ctx.tabs.duplicate_active();
    ctx.panes = crate::panes::PaneLayout::new(duplicate_a);
    ctx.panes.split_right(dirty_duplicate_b, 0);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_tab_close_duplicate_files(handle), 1);
    assert_eq!(ctx.tabs.count(), 4);
    assert_eq!(ctx.tabs.active(), 1);
    assert_eq!(ctx.panes.tab_at(0), Some(1));
    assert_eq!(ctx.panes.tab_at(1), Some(3));
    assert!(ctx.tabs.get(3).unwrap().is_dirty());
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Closed 2 duplicate tabs"
    );

    assert_eq!(crate::mui_tab_close_duplicate_files(handle), -1);
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "No duplicate file tabs");
    assert_eq!(ctx.tabs.get(2).unwrap().basename(), "b.mty");
    assert_ne!(b_idx, duplicate_b);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reload_active_file_refreshes_clean_file_and_protects_dirty_tab() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_reload_file_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("reload_me.mty");
    std::fs::write(&path, "old").unwrap();
    let idx = ctx.tabs.open_path(path.clone());
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    std::fs::write(&path, "new").unwrap();
    assert_eq!(crate::mui_tab_reload_active(handle), idx as i32);
    assert_eq!(String::from_utf8(ctx.tabs.active_model().to_bytes()).unwrap(), "new");
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reloaded reload_me.mty");

    ctx.tabs.active_model_mut().set_text_preserving_cursor("dirty local");
    ctx.tabs.set_dirty(idx, true);
    std::fs::write(&path, "external").unwrap();
    assert_eq!(crate::mui_tab_reload_active(handle), -1);
    assert_eq!(
        String::from_utf8(ctx.tabs.active_model().to_bytes()).unwrap(),
        "dirty local"
    );
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Save or discard changes before reloading"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn revert_active_file_discards_dirty_buffer_from_disk() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_revert_file_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("revert_me.mty");
    std::fs::write(&path, "disk").unwrap();
    let idx = ctx.tabs.open_path(path.clone());
    ctx.tabs.active_model_mut().set_text_preserving_cursor("dirty local");
    ctx.tabs.set_dirty(idx, true);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    std::fs::write(&path, "external").unwrap();
    assert_eq!(crate::mui_tab_revert_active(handle), idx as i32);
    assert_eq!(ctx.tabs.active(), idx);
    assert!(!ctx.tabs.is_dirty(idx));
    assert_eq!(
        String::from_utf8(ctx.tabs.active_model().to_bytes()).unwrap(),
        "external"
    );
    assert_eq!(ctx.toasts.toasts().last().unwrap().message, "Reverted revert_me.mty");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn toast_clear_abi_dismisses_visible_notifications() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    assert_eq!(crate::mui_toast_clear(handle), 0);
    ctx.push_toast(crate::toast::Kind::Info, "First");
    ctx.push_toast(crate::toast::Kind::Warn, "Second");
    assert_eq!(ctx.toasts.len(), 2);
    assert_eq!(crate::mui_toast_clear(handle), 1);
    assert!(ctx.toasts.is_empty());
    assert_eq!(crate::mui_toast_clear(handle), 0);
}

#[test]
fn panel_switch_clears_low_priority_toasts_only() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    ctx.active_panel = crate::PANEL_EXPLORER;
    ctx.push_toast(crate::toast::Kind::Info, "Old info");
    ctx.push_toast(crate::toast::Kind::Success, "Old success");
    ctx.push_toast(crate::toast::Kind::Warn, "Keep warning");
    ctx.push_toast(crate::toast::Kind::Error, "Keep error");

    assert_eq!(crate::panels::mui_panel_set(handle, crate::PANEL_SEARCH), crate::PANEL_SEARCH);

    let remaining: Vec<_> = ctx
        .toasts
        .toasts()
        .iter()
        .map(|toast| (toast.kind, toast.message.as_str()))
        .collect();
    assert_eq!(
        remaining,
        vec![
            (crate::toast::Kind::Warn, "Keep warning"),
            (crate::toast::Kind::Error, "Keep error"),
        ]
    );
}

#[test]
fn compact_windows_show_at_most_two_toast_cards() {
    assert_eq!(crate::toast::visible_toast_count(560, 520, 0.0), 2);
    assert_eq!(crate::toast::visible_toast_count(900, 700, 0.0), crate::toast::MAX_VISIBLE);
    assert_eq!(
        crate::toast::visible_toast_count(900, 700, crate::layout::term_panel_height(700)),
        2,
        "a bottom dock should reduce toast stack dominance"
    );
}

#[test]
fn toast_click_abi_dismisses_hit_toast_and_consumes_only_hits() {
    use crate::ffi::MuiEvent;

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    ctx.push_toast(crate::toast::Kind::Info, "Old");
    ctx.push_toast(crate::toast::Kind::Warn, "New");
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        10.0,
        10.0,
        0,
    );
    assert_eq!(crate::mui_toast_click(handle), 0);
    assert_eq!(ctx.toasts.len(), 2);

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        760.0,
        530.0,
        0,
    );
    assert_eq!(crate::mui_toast_click(handle), 1);
    assert_eq!(ctx.toasts.len(), 1);
    assert_eq!(ctx.toasts.toasts()[0].message, "Old");

    ctx.gpu.width = 1280;
    ctx.gpu.height = 832;
    ctx.gpu.phys_width = 1280;
    ctx.gpu.phys_height = 832;
    crate::uiscale::set_os_scale(1.375);
    crate::uiscale::set_user_zoom(1.0);
    ctx.push_toast(crate::toast::Kind::Info, "Scaled");
    let visible_w = crate::layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width) as f32;
    let visible_h = crate::layout::visible_height(ctx.gpu.height, ctx.gpu.phys_height) as f32;
    let card_w = 320.0_f32.min(visible_w - 36.0 - 96.0).max(180.0);
    let card_x = (visible_w - 18.0 - 96.0 - card_w).max(18.0);
    let bottom = visible_h - 18.0 - crate::theme::LINE_HEIGHT();
    let card_y = bottom - 56.0;
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        card_x + card_w - 20.0,
        card_y + 24.0,
        0,
    );
    assert_eq!(crate::mui_toast_click(handle), 1);
    assert_eq!(ctx.toasts.len(), 1);
    assert_eq!(ctx.toasts.toasts()[0].message, "Old");
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
}

#[test]
fn modal_overlays_suppress_toast_draw_and_click_targets() {
    use crate::ffi::MuiEvent;

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 600;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    ctx.push_toast(crate::toast::Kind::Success, "Opened folder: project");
    ctx.settings_panel.open();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        760.0,
        530.0,
        0,
    );
    assert_eq!(crate::mui_toast_click(handle), 0);
    assert_eq!(ctx.toasts.len(), 1);

    crate::mui_toast_draw(handle);
    assert!(
        ctx.rects_overlay.is_empty(),
        "toast draw should not paint cards over active modal overlays"
    );
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
}

#[test]
fn command_overlays_suppress_toast_draw_and_click_targets() {
    use crate::ffi::MuiEvent;

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 1280;
    ctx.gpu.height = 832;
    ctx.gpu.phys_width = 1280;
    ctx.gpu.phys_height = 832;
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.push_toast(crate::toast::Kind::Success, "Command overlay toast");
    crate::mui_palette_open(handle);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        1000.0,
        720.0,
        0,
    );
    assert_eq!(crate::mui_toast_click(handle), 0);
    assert_eq!(ctx.toasts.len(), 1);

    crate::mui_toast_draw(handle);
    assert!(
        ctx.rects_overlay.is_empty(),
        "toast draw should not paint cards over active command palette overlays"
    );
    crate::mui_palette_cancel(handle);

    crate::mui_quickopen_open(handle);
    crate::mui_toast_draw(handle);
    assert!(
        ctx.rects_overlay.is_empty(),
        "toast draw should not paint cards over active Quick Open overlays"
    );
    assert_eq!(crate::mui_toast_click(handle), 0);
    assert_eq!(ctx.toasts.len(), 1);

    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
}

#[test]
fn active_file_os_reveal_builds_platform_file_manager_command() {
    let path = std::path::Path::new("C:\\workspace\\src\\main.mty");
    let Some((program, args)) = crate::abi::platform_reveal_command(path) else {
        return;
    };

    #[cfg(target_os = "windows")]
    {
        assert_eq!(program, "explorer.exe");
        assert_eq!(args, vec!["/select,C:\\workspace\\src\\main.mty"]);
    }
    #[cfg(target_os = "macos")]
    {
        assert_eq!(program, "open");
        assert_eq!(args, vec!["-R", "C:\\workspace\\src\\main.mty"]);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        assert_eq!(program, "xdg-open");
        assert!(!args.is_empty());
    }
}

#[test]
fn active_file_copy_path_builds_platform_clipboard_command() {
    let Some((program, args)) = crate::abi::platform_clipboard_command() else {
        return;
    };

    #[cfg(target_os = "windows")]
    {
        assert_eq!(program, "powershell");
        assert!(args.iter().any(|arg| arg.contains("Set-Clipboard")));
    }
    #[cfg(target_os = "macos")]
    {
        assert_eq!(program, "pbcopy");
        assert!(args.is_empty());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        assert!(program == "wl-copy" || program == "xclip");
    }
}

#[test]
fn active_file_relative_path_uses_workspace_root_and_slashes() {
    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join(format!("mui_relative_path_{}", std::process::id()));
    let file = root.join("src").join("main.mty");
    ctx.workspace.set_root(root.clone());
    ctx.tree.set_root(root.clone());

    assert_eq!(
        crate::abi::active_relative_path_text(&ctx, &file),
        "src/main.mty"
    );

    let outside = std::env::temp_dir().join("elsewhere.mty");
    assert_eq!(
        crate::abi::active_relative_path_text(&ctx, &outside),
        outside.to_string_lossy().replace('\\', "/")
    );
}

#[test]
fn active_file_name_and_directory_text_are_clipboard_ready() {
    let path = std::path::Path::new("C:\\workspace\\src\\main.mty");

    assert_eq!(crate::abi::active_file_name_text(path), "main.mty");
    assert_eq!(
        crate::abi::active_directory_text(path),
        "C:/workspace/src"
    );
    assert_eq!(
        crate::abi::active_directory_text(std::path::Path::new("scratch.mty")),
        "."
    );
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
    ctx.gpu.width = 1200;
    ctx.gpu.height = 600;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let controls_x = crate::titlebar::controls_x(ctx.gpu.width as f32);
    let strip_x = controls_x - crate::titlebar::ACTION_STRIP_W;
    let run_x = controls_x - 60.0 + 8.0;
    let menu_x = controls_x - 60.0 + 32.0;
    let body_left = crate::layout::body_left(ctx.sidebar_visible);
    let tab_right = strip_x;
    let command_x = (body_left + crate::layout::TAB_W + 14.0 + tab_right - 14.0) * 0.5;

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, run_x, 4.0, 0);
    assert_eq!(mui_topbar_action_at_click(handle), 1);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        strip_x + 3.0,
        4.0,
        0,
    );
    assert_eq!(
        mui_topbar_action_at_click(handle),
        1,
        "left padding in the action strip should not fall through to the editor"
    );
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, menu_x, 4.0, 0);
    assert_eq!(mui_topbar_action_at_click(handle), 2);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, command_x, 14.0, 0);
    assert_eq!(
        mui_topbar_action_at_click(handle),
        3,
        "the visible command-center pill should open Quick Open"
    );
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        strip_x + 31.0,
        4.0,
        0,
    );
    assert_eq!(
        mui_topbar_action_at_click(handle),
        2,
        "the gap between Run and More should open More, not type into the editor"
    );
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        controls_x - 2.0,
        4.0,
        0,
    );
    assert_eq!(
        mui_topbar_action_at_click(handle),
        2,
        "the dead gap before native window controls should still open More"
    );
    ctx.last_event.y = crate::layout::TAB_BAR_H + 1.0;
    assert_eq!(mui_topbar_action_at_click(handle), 0);

    crate::layout::set_zen(true);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, run_x, 4.0, 0);
    assert_eq!(mui_topbar_action_at_click(handle), 0);

    crate::layout::set_zen(before);
}

#[test]
fn explorer_header_actions_hit_their_visible_buttons() {
    use crate::ffi::MuiEvent;
    use crate::mui_explorer_header_at_click;

    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = true;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let centers = crate::abi::explorer_header_action_centers(crate::layout::RAIL_W, crate::layout::sidebar_w());

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, centers[0].0 + 7.5, 20.0, 0);
    assert_eq!(mui_explorer_header_at_click(handle), 1);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, centers[1].0 + 7.5, 20.0, 0);
    assert_eq!(mui_explorer_header_at_click(handle), 2);
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, centers[2].0 + 7.5, 20.0, 0);
    assert_eq!(mui_explorer_header_at_click(handle), 3);
    assert!(crate::abi::explorer_header_action_opens_dialog(1));
    assert!(crate::abi::explorer_header_action_opens_dialog(2));
    assert!(!crate::abi::explorer_header_action_opens_dialog(3));

    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, centers[0].0 + 7.5, 42.0, 0);
    assert_eq!(mui_explorer_header_at_click(handle), 0);
}

#[test]
fn explorer_row_name_fits_before_git_badge() {
    let mut ctx = ctx_or_skip!();
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();
    let chrome = crate::theme::CHROME_FONT_SIZE - 1.0;
    let name_x = sx + 12.0 + 14.0 + 17.0;
    let shown = crate::abi::fit_explorer_name(
        &mut ctx.text,
        "README.md",
        name_x,
        sx,
        sw,
        chrome,
        true,
    );
    let (shown_w, _) = ctx.text.measure_ui_sized(&shown, chrome);
    let (badge_w, _) = ctx.text.measure_ui_sized("U", chrome - 2.0);
    assert!(
        name_x + shown_w + 8.0 <= sx + sw - 22.0,
        "explorer name should leave a gap before the git badge: {shown}"
    );
    assert!(sx + sw - 22.0 + badge_w <= sx + sw - 8.0);

    let long = crate::abi::fit_explorer_name(
        &mut ctx.text,
        "a-very-long-readable-file-name-that-should-tail-ellipsize-before-the-badge.mty",
        name_x,
        sx,
        sw,
        chrome,
        true,
    );
    let (long_w, _) = ctx.text.measure_ui_sized(&long, chrome);
    assert!(name_x + long_w + 8.0 <= sx + sw - 22.0);
    assert!(long.starts_with('\u{2026}'));
}

#[test]
fn explorer_header_fits_before_action_buttons() {
    let mut ctx = ctx_or_skip!();
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::SIDEBAR_MIN_W;
    let chrome = crate::theme::CHROME_FONT_SIZE - 2.0;
    let shown =
        crate::abi::fit_explorer_header(&mut ctx.text, "MIGHTY-IDE-WIN64", sx, sw, chrome);
    let (shown_w, _) = ctx.text.measure_ui_sized(&shown, chrome);
    let label_x = sx + 14.0;
    let first_button_x = crate::abi::explorer_header_action_centers(sx, sw)[0].0 - 2.5;
    assert!(
        label_x + shown_w <= first_button_x - 8.0,
        "header should leave a visible gap before actions: {shown}"
    );
    assert!(shown.ends_with('\u{2026}'));

    let wide = crate::abi::fit_explorer_header(&mut ctx.text, "MIGHTY", sx, 248.0, chrome);
    assert!(!wide.ends_with('\u{2026}'));
}

#[test]
fn explorer_row_selection_handles_missing_active_path() {
    let file = std::path::Path::new("src/main.mty");

    assert!(!crate::abi::explorer_row_selected(false, file, None));
    assert!(!crate::abi::explorer_row_selected(true, file, Some(file)));
    assert!(crate::abi::explorer_row_selected(false, file, Some(file)));
    assert!(!crate::abi::explorer_row_selected(
        false,
        file,
        Some(std::path::Path::new("src/lib.mty"))
    ));
}

#[test]
fn scm_row_name_and_dir_fit_before_stage_action() {
    let mut ctx = ctx_or_skip!();
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let name_x = sx + 47.0;
    let action_left = sx + sw - 30.0;

    let name = crate::panels::fit_tail_px(
        &mut ctx.text,
        "very-long-source-file-name-that-should-not-run-under-stage-action.mty",
        action_left - name_x - 72.0,
        chrome,
    );
    let (name_w, _) = ctx.text.measure_ui_sized(&name, chrome);
    assert!(name_x + name_w + 8.0 <= action_left);
    assert!(name.starts_with('\u{2026}'));

    let dir_x = name_x + name_w + 6.0;
    let dir = crate::panels::fit_tail_px(
        &mut ctx.text,
        "src/deeply/nested/module/path",
        action_left - dir_x - 8.0,
        chrome - 1.5,
    );
    let (dir_w, _) = ctx.text.measure_ui_sized(&dir, chrome - 1.5);
    assert!(dir_x + dir_w + 8.0 <= action_left);
}

#[test]
fn scm_open_row_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_scm_open_row(handle, -1), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No source control row selected");

    ctx.scm.status.entries.push(crate::scm::ScmEntry {
        path: "deleted.mty".to_string(),
        staged: false,
        status: 'D',
    });
    assert_eq!(crate::panels::mui_scm_open_row(handle, 0), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Source control root missing");

    let root = std::env::temp_dir().join(format!("mui_scm_open_missing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    ctx.scm.root = Some(root.clone());
    assert_eq!(crate::panels::mui_scm_open_row(handle, 0), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Source control target missing: deleted.mty");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn scm_header_fits_before_action_buttons() {
    let mut ctx = ctx_or_skip!();
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::SIDEBAR_MIN_W;
    let chrome = crate::theme::CHROME_FONT_SIZE - 2.0;
    let title = crate::panels::scm_header_title_for_budget(&mut ctx.text, sx, sw, chrome);
    assert_eq!(title, "SCM");
    let shown = crate::panels::fit_scm_header(&mut ctx.text, title, sx, sw, chrome);
    let (shown_w, _) = ctx.text.measure_ui_sized(&shown, chrome);
    let label_x = sx + 14.0;
    let first_button_x = sx + sw - 94.0;
    assert!(
        label_x + shown_w <= first_button_x - 8.0,
        "SCM header should leave a visible gap before actions: {shown}"
    );
    assert!(
        !shown.ends_with('\u{2026}'),
        "compact SCM header should use a complete title instead of truncating: {shown}"
    );

    let wide_title = crate::panels::scm_header_title_for_budget(&mut ctx.text, sx, 248.0, chrome);
    assert_eq!(wide_title, "SOURCE CONTROL");
    let wide = crate::panels::fit_scm_header(&mut ctx.text, wide_title, sx, 248.0, chrome);
    assert!(!wide.ends_with('\u{2026}'));
}

#[test]
fn scm_section_branch_budget_yields_to_changes_count() {
    let mut ctx = ctx_or_skip!();
    let sx = crate::layout::RAIL_W;
    let chrome = crate::theme::CHROME_FONT_SIZE - 2.0;
    let compact_budget = crate::panels::scm_section_branch_budget(
        &mut ctx.text,
        sx,
        crate::layout::SIDEBAR_MIN_W,
        123,
        123,
        45,
        chrome,
    );
    assert!(
        compact_budget < 24.0,
        "compact SCM section should hide branch instead of crossing count: {compact_budget}"
    );

    let wide_budget =
        crate::panels::scm_section_branch_budget(&mut ctx.text, sx, 248.0, 3, 0, 0, chrome);
    assert!(
        wide_budget >= 24.0,
        "wide SCM section should still have room for branch context: {wide_budget}"
    );
}

#[test]
fn branch_picker_visible_rows_fit_compact_heights() {
    assert_eq!(crate::panels::branch_picker_visible_rows(720, 0), 0);
    assert_eq!(crate::panels::branch_picker_visible_rows(120, 10), 1);
    assert_eq!(crate::panels::branch_picker_visible_rows(220, 10), 3);
    assert_eq!(crate::panels::branch_picker_visible_rows(720, 10), 10);
}

#[test]
fn branch_picker_geometry_keeps_positive_width_in_narrow_windows() {
    let (box_x, _box_y, box_w, _box_h, _list_top, _row_h) =
        crate::panels::branch_picker_geometry(120, 220, 3);
    assert!(box_w > 0.0, "branch picker card width should remain positive");
    assert!(box_x >= 0.0, "branch picker card should not start offscreen");
    assert!(
        box_x + box_w <= 120.0,
        "branch picker card should fit narrow windows: x={box_x} w={box_w}"
    );

    let (wide_x, _wide_y, wide_w, _wide_h, _wide_list_top, _wide_row_h) =
        crate::panels::branch_picker_geometry(320, 220, 3);
    assert!(wide_x >= 0.0);
    assert!(wide_w <= 288.0);
    assert!(wide_x + wide_w <= 320.0);
}

#[test]
fn branch_accept_without_picker_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::panels::mui_branch_accept(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No branch picker open");
}

#[test]
fn debug_header_title_fits_before_state_pill() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(520);

    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();
    let chrome = crate::theme::CHROME_FONT_SIZE - 2.0;
    let pill_w = crate::dapabi::debug_state_pill_width(&mut ctx.text, "running\u{2026}", chrome);
    let pill_x = sx + sw - pill_w - 12.0;
    let title_x = sx + 34.0;
    let title = crate::dapabi::debug_header_title_for_budget(&mut ctx.text, title_x, pill_x, chrome);
    assert_eq!(title, "DEBUG");
    let shown =
        crate::dapabi::fit_debug_header_title(&mut ctx.text, title, title_x, pill_x, chrome);
    let (shown_w, _) = ctx.text.measure_ui_sized(&shown, chrome);
    assert!(
        title_x + shown_w <= pill_x - 8.0,
        "debug header should leave a visible gap before the state pill: {shown}"
    );
    assert_eq!(shown, "D\u{2009}E\u{2009}B\u{2009}U\u{2009}G\u{2009}");

    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(1200);
    let wide_sw = crate::layout::sidebar_w();
    let wide_pill_x = sx + wide_sw - pill_w - 12.0;
    assert_eq!(
        crate::dapabi::debug_header_title_for_budget(&mut ctx.text, title_x, wide_pill_x, chrome),
        "RUN AND DEBUG"
    );
}

#[test]
fn debug_toolbar_fits_compact_sidebar() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(520);

    let tb = crate::dapabi::toolbar_geom();
    let toolbar_right = tb.x0 + 5.0 * tb.btn + 4.0 * tb.gap;
    assert!(
        toolbar_right <= crate::layout::sidebar_right() - 12.0,
        "debug toolbar should stay inside compact sidebar: right={toolbar_right}"
    );

    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(900);
}

#[test]
fn debug_toolbar_play_starts_or_prompts_from_idle() {
    use crate::ffi::MuiEvent;

    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(900);
    ctx.sidebar_visible = true;
    ctx.active_panel = crate::PANEL_DEBUG;
    ctx.dbg.set_open(true);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let tb = crate::dapabi::toolbar_geom();

    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        tb.x0 + tb.btn * 0.5,
        tb.y + tb.btn * 0.5,
        0,
    );
    let hit = crate::dapabi::mui_dbg_click(handle);
    assert_eq!(hit, 1000);

    crate::dapabi::mui_dbg_toolbar_action(handle, hit);
    assert_eq!(crate::dapabi::mui_dbg_active(handle), 1);
    assert_eq!(crate::dapabi::mui_dbg_state(handle), crate::dap::DebugState::Idle.as_i32());

    crate::layout::reset_sidebar_preset();
}

#[test]
fn debug_restart_without_target_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::dapabi::mui_dbg_restart(handle), 0);
    assert_eq!(ctx.active_panel, crate::PANEL_DEBUG);
    assert!(ctx.sidebar_visible);
    assert_eq!(crate::dapabi::mui_dbg_active(handle), 1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No debug target to restart");
}

#[test]
fn debug_start_without_active_file_opens_visible_debug_view() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::dapabi::mui_dbg_start(handle), crate::dap::DebugState::Idle.as_i32());
    assert_eq!(ctx.active_panel, crate::PANEL_DEBUG);
    assert!(ctx.sidebar_visible);
    assert_eq!(crate::dapabi::mui_dbg_active(handle), 1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Open a file before starting debug");
}

#[test]
fn direct_debug_actions_report_unavailable_state() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::dapabi::mui_dbg_stop(handle);
    assert_eq!(ctx.active_panel, crate::PANEL_DEBUG);
    assert!(ctx.sidebar_visible);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "No debug session to stop"
    );

    crate::dapabi::mui_dbg_pause(handle);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Pause is available while running"
    );

    assert_eq!(crate::dapabi::mui_dbg_continue(handle), crate::dap::DebugState::Idle.as_i32());
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Continue is available when paused"
    );

    crate::dapabi::mui_dbg_step_over(handle);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Step Over is available when paused"
    );

    crate::dapabi::mui_dbg_step_into(handle);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Step Into is available when paused"
    );

    crate::dapabi::mui_dbg_step_out(handle);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Step Out is available when paused"
    );
}

#[test]
fn debug_step_from_closed_sidebar_reveals_debug_view() {
    let mut ctx = ctx_or_skip!();
    ctx.dbg.seed_demo("C:/workspace/src/main.mty");
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::dapabi::mui_dbg_step_over(handle);
    assert_eq!(ctx.active_panel, crate::PANEL_DEBUG);
    assert!(ctx.sidebar_visible);
    assert_eq!(crate::dapabi::mui_dbg_active(handle), 1);
    assert!(ctx.toasts.toasts().is_empty());
}

#[test]
fn debug_toolbar_actions_reuse_direct_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    crate::dapabi::mui_dbg_toolbar_action(
        handle,
        1000 + crate::dapabi::TB_STEP_OVER,
    );
    assert_eq!(ctx.active_panel, crate::PANEL_DEBUG);
    assert!(ctx.sidebar_visible);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Step Over is available when paused"
    );

    crate::dapabi::mui_dbg_toolbar_action(
        handle,
        1000 + crate::dapabi::TB_STEP_INTO,
    );
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Step Into is available when paused"
    );

    crate::dapabi::mui_dbg_toolbar_action(
        handle,
        1000 + crate::dapabi::TB_STEP_OUT,
    );
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Step Out is available when paused"
    );

    crate::dapabi::mui_dbg_toolbar_action(
        handle,
        1000 + crate::dapabi::TB_STOP,
    );
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "No debug session to stop"
    );
}

#[test]
fn breakpoint_toggle_without_file_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    ctx.active_panel = crate::PANEL_EXPLORER;
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::dapabi::mui_bp_toggle(handle, 0), 0);
    assert_eq!(ctx.active_panel, crate::PANEL_DEBUG);
    assert!(ctx.sidebar_visible);
    assert_eq!(crate::dapabi::mui_dbg_active(handle), 1);
    assert_eq!(crate::dapabi::mui_bp_count(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Save the file before setting breakpoints");
}

#[test]
fn debug_stack_name_fits_before_location() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(520);

    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let loc = crate::dapabi::fit_debug_stack_location(&mut ctx.text, "demo.mty:300", sw * 0.42, chrome - 1.5);
    let loc_w = ctx.text.measure_ui_sized(&loc, chrome - 1.5).0;
    let loc_x = sx + sw - loc_w - 14.0;
    let name_x = sx + 30.0;
    let name = crate::dapabi::fit_debug_stack_name(
        &mut ctx.text,
        "compute_sum_with_a_long_debug_frame_name",
        name_x,
        loc_x,
        chrome,
    );
    let (name_w, _) = ctx.text.measure_ui_sized(&name, chrome);
    assert!(
        name_x + name_w <= loc_x - 8.0,
        "debug stack frame name should leave a gap before location: {name}"
    );
    assert!(name.ends_with('\u{2026}'));

    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(900);
}

#[test]
fn debug_ui_text_width_uses_measured_text() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let short = crate::dapabi::debug_ui_text_width(&mut ctx.text, "i", chrome);
    let long = crate::dapabi::debug_ui_text_width(&mut ctx.text, "result_value", chrome);

    assert!(short > 0.0);
    assert!(long > short);
}

#[test]
fn debug_variable_equals_position_tracks_rendered_name_width() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let sx = crate::layout::RAIL_W;
    let name_x = sx + 16.0;
    let narrow = "i";
    let wide = "result_value";
    let adv = crate::layout::CHAR_W();
    let narrow_eq = name_x + crate::dapabi::debug_ui_text_width(&mut ctx.text, narrow, chrome) + adv;
    let wide_eq = name_x + crate::dapabi::debug_ui_text_width(&mut ctx.text, wide, chrome) + adv;

    assert!(wide_eq > narrow_eq);
    assert!(narrow_eq > name_x);
}

#[test]
fn debug_variable_name_fits_measured_budget() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let budget = crate::dapabi::debug_variable_name_budget(&mut ctx.text, 12, chrome);
    let shown = crate::dapabi::fit_debug_variable_name(
        &mut ctx.text,
        "long_variable_name_for_debugger",
        budget,
        chrome,
    );
    let shown_w = ctx.text.measure_ui_sized(&shown, chrome).0;

    assert!(shown.ends_with('\u{2026}'));
    assert!(shown_w <= budget + 0.5, "variable name should fit measured budget: {shown}");
}

#[test]
fn debug_variable_separator_advance_uses_measured_text() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let sep = crate::dapabi::debug_variable_separator_advance(&mut ctx.text, chrome);
    let measured = ctx.text.measure_ui_sized(" = ", chrome).0;
    let eq = ctx.text.measure_ui_sized("=", chrome).0;

    assert_eq!(sep, measured);
    assert!(sep > eq);
}

#[test]
fn debug_variable_value_reserves_measured_type_label() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();
    let name_x = sx + 16.0;
    let name_budget = crate::dapabi::debug_variable_name_budget(&mut ctx.text, 12, chrome);
    let name = crate::dapabi::fit_debug_variable_name(&mut ctx.text, "result_value", name_budget, chrome);
    let sep = crate::dapabi::debug_variable_separator_advance(&mut ctx.text, chrome);
    let eq_w = crate::dapabi::debug_ui_text_width(&mut ctx.text, "=", chrome);
    let space_w = ((sep - eq_w) * 0.5).max(0.0);
    let eq_x = name_x + crate::dapabi::debug_ui_text_width(&mut ctx.text, &name, chrome) + space_w;
    let val_x = eq_x + eq_w + space_w;
    let kind = "Array<String>";
    let kind_w = crate::dapabi::debug_ui_text_width(&mut ctx.text, kind, chrome - 2.0);
    let kind_x = sx + sw - kind_w - 12.0;
    let value_budget = (kind_x - 8.0 - val_x).max(0.0);
    let shown = crate::dapabi::fit_debug_variable_value(
        &mut ctx.text,
        "\"a very long debugger value that should stop before the type label\"",
        value_budget,
        chrome,
    );
    let shown_w = ctx.text.measure_ui_sized(&shown, chrome).0;

    assert!(shown.ends_with('\u{2026}'));
    assert!(
        val_x + shown_w <= kind_x - 8.0 + 0.5,
        "variable value should leave a measured gap before type metadata: {shown}"
    );
}

#[test]
fn debug_console_line_fits_measured_panel_width() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE - 0.5;
    let sx = crate::layout::RAIL_W;
    let sw = crate::layout::sidebar_w();
    let text_x = sx + 14.0;
    let max_w = (sx + sw - 12.0 - text_x).max(0.0);
    let shown = crate::dapabi::fit_debug_console_line(
        &mut ctx.text,
        "Debugger output: a very long diagnostic line with paths and runtime details",
        max_w,
        chrome,
    );
    let shown_w = ctx.text.measure_ui_sized(&shown, chrome).0;

    assert!(shown.ends_with('\u{2026}'));
    assert!(
        shown_w <= max_w + 0.5,
        "debug console line should fit measured panel width: {shown}"
    );
}

#[test]
fn quickopen_search_placeholder_fits_before_mode_pill() {
    let mut ctx = ctx_or_skip!();
    let placeholder = "Search files by name\u{2026}  (\u{203A} commands  @ symbols  : line)";
    let box_x = 40.0;
    let box_w = 480.0;
    let q_text_base_x = box_x + 50.0;
    let q_text_x = q_text_base_x + 10.0;
    let pill_w = 44.0;
    let pill_x = box_x + box_w - pill_w - 18.0;
    let budget = crate::quickopen::quickopen_query_text_budget(q_text_x, pill_x, true);
    let shown = crate::quickopen::fit_query_placeholder(&mut ctx.text, placeholder, budget, 16.0);
    let (shown_w, _) = ctx.text.measure_ui_sized(&shown, 16.0);
    assert!(
        q_text_x + shown_w <= pill_x - 30.0,
        "placeholder should leave a visible gap before mode pill: {shown}"
    );
    assert!(shown.ends_with('\u{2026}'));
    assert!(
        !shown.contains('('),
        "compact placeholder should not render a partial mode-hint sentence: {shown}"
    );
}

#[test]
fn run_status_label_stays_ascii_for_compact_chip() {
    assert_eq!(crate::featureabi::run_status_label(true, None, 0), "running");
    assert_eq!(
        crate::featureabi::run_status_label(false, Some(1), 142),
        "exit 1"
    );
    assert!(
        crate::featureabi::run_status_label(false, Some(0), 7).is_ascii(),
        "compact run status should avoid missing glyphs"
    );
}

#[test]
fn run_header_status_pill_leaves_gap_after_run_label() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let g_x0 = crate::layout::region(true).left;
    let run_label_x = g_x0 + 32.0;
    let run_size = chrome - 1.0;
    let (run_w, _) = ctx.text.measure_ui_sized("RUN", run_size);
    let status = crate::featureabi::run_status_label(false, Some(1), 0);
    let (status_w, _) = ctx.text.measure_ui_sized(&status, chrome - 2.0);
    let pill_w = status_w + 22.0;
    let right_edge = crate::layout::dock_header_content_right(560, 520);
    let preferred_x = right_edge - pill_w;
    let min_x = run_label_x + run_w + 12.0;
    let sx = crate::featureabi::run_status_pill_x(preferred_x, min_x, right_edge, pill_w);

    assert!(
        sx >= min_x,
        "status pill should leave a readable gap after RUN: sx={sx} min={min_x}"
    );
}

#[test]
fn run_output_line_fits_compact_panel_width() {
    let mut ctx = ctx_or_skip!();
    let line = "[MT2001] Error: expected `I32` but found a deliberately verbose expression";
    let shown = crate::featureabi::fit_code_text(&mut ctx.text, line, 210.0, crate::theme::CHROME_FONT_SIZE);

    assert!(shown.ends_with('\u{2026}'), "long run output should visibly ellipsize: {shown}");
    assert!(
        ctx.text.measure_sized(&shown, crate::theme::CHROME_FONT_SIZE).0 <= 210.0,
        "fitted run output must not draw under the dock edge: {shown}"
    );
    assert!(
        shown.starts_with("[MT2001]"),
        "diagnostic fitting should preserve the error code prefix: {shown}"
    );
}

#[test]
fn editor_gutter_number_width_uses_rendered_code_text() {
    let mut ctx = ctx_or_skip!();
    let size = crate::theme::CHROME_FONT_SIZE;
    let short = crate::abi::gutter_number_width(&mut ctx.text, "9", size);
    let wide = crate::abi::gutter_number_width(&mut ctx.text, "1000", size);
    let measured = ctx.text.measure_sized("1000", size).0;

    assert!(short > 0.0);
    assert!(wide > short);
    assert_eq!(wide, measured);
}

#[test]
fn folded_indicator_width_uses_measured_label_text() {
    let mut ctx = ctx_or_skip!();
    let size = crate::theme::CHROME_FONT_SIZE - 1.0;
    let compact = crate::abi::folded_indicator_width(&mut ctx.text, "... 1 line", size);
    let wide = crate::abi::folded_indicator_width(&mut ctx.text, "... 128 lines", size);
    let measured = ctx.text.measure_ui_sized("... 128 lines", size).0 + 12.0;

    assert!(compact > 12.0);
    assert!(wide > compact);
    assert_eq!(wide, measured);
}

#[test]
fn diff_summary_width_uses_measured_ui_text() {
    let mut ctx = ctx_or_skip!();
    let size = crate::theme::CHROME_FONT_SIZE - 1.0;
    let short = crate::featureabi::feature_ui_text_width(&mut ctx.text, "Staged   +1 \u{2212}0", size);
    let long = crate::featureabi::feature_ui_text_width(
        &mut ctx.text,
        "Working Tree   +128 \u{2212}64   esc to close",
        size,
    );

    assert!(short > 0.0);
    assert!(long > short);
}

#[test]
fn diff_hunk_button_width_uses_measured_label_text() {
    let mut ctx = ctx_or_skip!();
    let size = crate::theme::CHROME_FONT_SIZE - 1.0;
    let compact = crate::featureabi::diff_hunk_button_width(&mut ctx.text, "+ Stage", size);
    let wide = crate::featureabi::diff_hunk_button_width(&mut ctx.text, "\u{2212} Unstage hunk", size);

    assert!(compact > 18.0);
    assert!(wide > compact);
}

#[test]
fn diff_hunk_header_fits_before_measured_action_button() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let hunk_x = 50.0;
    let button_x = 245.0;
    let budget = (button_x - hunk_x - 10.0_f32).max(0.0);
    let shown = crate::featureabi::fit_diff_code_text(
        &mut ctx.text,
        "@@ -120,24 +120,42 @@ fn render_really_long_inline_diff_header_context()",
        budget,
        chrome,
    );
    let shown_w = ctx.text.measure_sized(&shown, chrome).0;

    assert!(shown.ends_with('\u{2026}'));
    assert!(
        hunk_x + shown_w <= button_x - 10.0 + 0.5,
        "hunk header should leave measured space before action button: {shown}"
    );
}

#[test]
fn diff_body_line_fits_measured_editor_width() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let budget = 220.0;
    let shown = crate::featureabi::fit_diff_code_text(
        &mut ctx.text,
        "+    let very_long_identifier = render_really_long_inline_diff_body_line();",
        budget,
        chrome,
    );
    let shown_w = ctx.text.measure_sized(&shown, chrome).0;

    assert!(shown.ends_with('\u{2026}'));
    assert!(
        shown_w <= budget + 0.5,
        "diff body line should fit measured budget: {shown}"
    );
}

#[test]
fn blame_annotation_fits_measured_window_budget() {
    let mut ctx = ctx_or_skip!();
    let chrome = crate::theme::CHROME_FONT_SIZE - 1.5;
    let budget = 150.0;
    let shown = crate::featureabi::fit_blame_text(
        &mut ctx.text,
        "\u{2022} Very Long Contributor Name \u{00b7} 2026-06-03 \u{00b7} abcdef0",
        budget,
        chrome,
    );
    let shown_w = ctx.text.measure_ui_sized(&shown, chrome).0;

    assert!(shown.ends_with('\u{2026}'));
    assert!(
        shown_w <= budget + 0.5,
        "blame annotation should fit measured budget: {shown}"
    );
}

#[test]
fn blame_close_reports_feedback_and_is_idempotent() {
    let mut ctx = ctx_or_skip!();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    let blob = "\
1111111111111111111111111111111111111111 1 1 1
author Ada Lovelace
author-time 1136239445
author-tz +0000
filename src/main.mty
\tfn main() {}
";
    assert_eq!(ctx.blame.seed_demo(blob), 1);
    assert_eq!(crate::featureabi::mui_blame_active(handle), 1);

    assert_eq!(crate::featureabi::mui_blame_close(handle), 1);
    assert_eq!(crate::featureabi::mui_blame_active(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Blame hidden");

    assert_eq!(crate::featureabi::mui_blame_close(handle), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Blame is already hidden");
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
fn status_problems_chip_uses_readable_labels_when_width_allows() {
    use crate::mui_status_render;

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 1280;
    ctx.gpu.height = 720;
    ctx.scm.status.branch = "main".to_string();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    mui_status_render(handle, 4);
    let (_, _, w, _) = ctx
        .status_problems_rect
        .expect("wide status bars should render the Problems chip");

    assert!(
        w > 85.0,
        "wide status bars should use readable 'err'/'warn' labels, not bare numbers: w={w}"
    );
}

#[test]
fn status_bar_compacts_long_left_cluster_before_right_cluster() {
    use crate::{mui_status_problems_chip_at_click, mui_status_render};

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 560;
    ctx.gpu.height = 600;
    ctx.scm.status.branch =
        "feature/very-long-branch-name-with-wide-ui-letters-and-a-ticket-number-12345".to_string();
    ctx.scm.status.ahead = 123;
    ctx.scm.status.behind = 45;
    ctx.status_cursor = (12345, 678);
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    mui_status_render(handle, 987);
    if let Some((x, y, w, h)) = ctx.status_problems_rect {
        assert!(x >= 0.0);
        assert!(x + w < ctx.gpu.width as f32 - 8.0);
        ctx.last_event = crate::ffi::MuiEvent::mouse(
            crate::ffi::MUI_EVENT_MOUSE_DOWN,
            0,
            x + w * 0.5,
            y + h * 0.5,
            0,
        );
        assert_eq!(mui_status_problems_chip_at_click(handle), 1);
    }
}

#[test]
fn peek_header_label_fits_measured_budget() {
    let mut ctx = ctx_or_skip!();
    let label = "very_long_nested_file_name_for_peek_header.mty:128";
    let fitted = crate::peek::fit_peek_header_label(&mut ctx.text, label, 92.0, crate::theme::CHROME_FONT_SIZE);
    assert!(fitted.ends_with('\u{2026}'));
    assert!(
        ctx.text.measure_ui_sized(&fitted, crate::theme::CHROME_FONT_SIZE).0 <= 92.0,
        "peek header should fit its measured budget: {fitted}"
    );

    let short = crate::peek::fit_peek_header_label(&mut ctx.text, "main.mty:1", 160.0, crate::theme::CHROME_FONT_SIZE);
    assert_eq!(short, "main.mty:1");
}

#[test]
fn peek_close_clears_inline_view() {
    let mut ctx = ctx_or_skip!();
    assert!(ctx.peek.open_at(
        std::path::PathBuf::from("src/main.mty"),
        0,
        0,
        2,
        crate::langdetect::Language::Mighty,
        Some("fn target() {}\n")
    ));
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;
    assert_eq!(crate::stickyabi::mui_peek_active(handle), 1);
    crate::stickyabi::mui_peek_close(handle);
    assert_eq!(crate::stickyabi::mui_peek_active(handle), 0);
    assert_eq!(crate::stickyabi::mui_peek_line_count(handle), 0);
}

#[test]
fn visible_surface_size_honors_screenshot_caps() {
    let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("MUI_SCREENSHOT_W", "560");
    std::env::set_var("MUI_SCREENSHOT_H", "520");
    let size = crate::abi::visible_surface_size_for(640, 0, 600, 0);
    std::env::remove_var("MUI_SCREENSHOT_W");
    std::env::remove_var("MUI_SCREENSHOT_H");
    assert_eq!(size, (560, 520));
}

#[test]
fn status_resize_grip_stays_in_bottom_right_corner() {
    let (x, y, w, h) = crate::abi::status_resize_grip_rect(1280.0, 832.0);

    assert!(x >= 1254.0, "grip should be visually anchored at the right edge");
    assert!(y >= 808.0, "grip should stay inside the status bar, not on top of text");
    assert!(x + w <= 1273.0, "grip should leave a frame margin for borderless resize");
    assert!(y + h <= 826.0, "grip should leave a frame margin for borderless resize");
    assert_eq!((w, h), (16.0, 16.0));
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
    assert_eq!(
        mui_chord_command_id(handle, 's' as i32, MOD_CTRL | MOD_ALT),
        crate::palette::CMD_SAVE_ALL as i32
    );
    assert_eq!(
        mui_chord_command_id(handle, 'b' as i32, MOD_CTRL | MOD_ALT),
        crate::palette::CMD_SIDEBAR_CYCLE_WIDTH as i32
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

#[test]
fn sidebar_cycle_width_dispatch_opens_and_rotates_presets() {
    use crate::mui_sidebar_layout_dispatch;

    let mut ctx = ctx_or_skip!();
    ctx.sidebar_visible = false;
    crate::layout::set_window_width(1280);
    crate::layout::reset_sidebar_preset();
    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(
        mui_sidebar_layout_dispatch(handle, crate::palette::CMD_SIDEBAR_CYCLE_WIDTH as i32),
        1
    );
    assert!(ctx.sidebar_visible, "cycle should reveal the sidebar if it was hidden");
    assert_eq!(ctx.active_panel, crate::PANEL_EXPLORER);
    assert_eq!(crate::layout::sidebar_preset(), 1);

    assert_eq!(
        mui_sidebar_layout_dispatch(handle, crate::palette::CMD_SIDEBAR_CYCLE_WIDTH as i32),
        3
    );
    assert_eq!(crate::layout::sidebar_preset(), 2);

    assert_eq!(
        mui_sidebar_layout_dispatch(handle, crate::palette::CMD_SIDEBAR_CYCLE_WIDTH as i32),
        2
    );
    assert_eq!(crate::layout::sidebar_preset(), 0);
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
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
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
    assert!(ctx.toasts.toasts().is_empty(), "successful undo should stay quiet");
    // After undo the '!' edit is gone (line 0 back to "hi").
    assert!(mui_ed_cursor_col(h) <= after);
    assert_eq!(mui_ed_redo(h), 1);
    assert!(ctx.toasts.toasts().is_empty(), "successful redo should stay quiet");
}

#[test]
fn editor_undo_redo_misses_report_visible_feedback() {
    use crate::{mui_ed_redo, mui_ed_undo};

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_ed_undo(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Nothing to undo");

    assert_eq!(mui_ed_redo(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Nothing to redo");
}

#[test]
fn editor_undo_redo_report_read_only_preview() {
    use crate::{mui_ed_redo, mui_ed_undo};

    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_undo_redo_read_only_preview");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("asset.bin");
    std::fs::write(&path, b"\0binary preview").unwrap();
    ctx.tabs.open_path(path);
    assert!(ctx.tabs.active_read_only());
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_ed_undo(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Undo is unavailable in read-only previews");

    assert_eq!(mui_ed_redo(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Redo is unavailable in read-only previews");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn codeaction_no_actions_toasts_feedback() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_codeaction_request(h, 0, 0), 0);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "No code actions available"
    );
}

#[test]
fn codeaction_command_without_file_toasts_feedback() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;
    ctx.codeaction.set(vec![crate::language::CodeAction {
        title: "Run server command".to_string(),
        edit: None,
        command_edit: None,
        command: Some(crate::language::CommandAction {
            command: "server.apply".to_string(),
            arguments_json: None,
        }),
        fix_all_mty: false,
    }]);

    assert_eq!(crate::mui_codeaction_apply(h), 0);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Code action needs a file"
    );
}

#[test]
fn format_current_reports_missing_or_unsupported_target() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_format_current(h), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Save the file before formatting");

    let path = std::env::temp_dir().join("mui_format_unsupported.txt");
    let original = b"plain text that must survive unsupported format\n";
    std::fs::write(&path, original).unwrap();
    ctx.tabs.open_path(path.clone());
    crate::sync_active_path(&mut ctx);

    assert_eq!(crate::mui_format_current(h), 0);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Format is available for Mighty files");

    let _ = std::fs::remove_file(path);
}

#[test]
fn navigation_requests_report_missing_targets() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_hover_request(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Save the file before hover");

    assert_eq!(crate::mui_def_request(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Save the file before Go to Definition");

    assert_eq!(crate::stickyabi::mui_peek_open(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Save the file before Peek Definition");

    assert_eq!(crate::abi::mui_sig_request(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Save the file before signature help");

    let path = std::env::temp_dir().join("mui_nav_plain_text.txt");
    std::fs::write(&path, b"plain text\n").unwrap();
    ctx.tabs.open_path(path.clone());
    crate::sync_active_path(&mut ctx);

    assert_eq!(crate::mui_hover_request(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No hover information");

    assert_eq!(crate::mui_def_request(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No definition found");

    assert_eq!(crate::stickyabi::mui_peek_open(h, 0, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "No definition found");

    let _ = std::fs::remove_file(path);
}

#[test]
fn definition_open_target_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_def_open_target(h), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No definition target selected");

    let root = std::env::temp_dir().join(format!("mui_def_missing_{}", std::process::id()));
    let missing = root.join("target.mty");
    ctx.def.set(Some(crate::nav::DefTarget {
        path: missing,
        line: 0,
        col: 0,
    }));

    assert_eq!(crate::mui_def_open_target(h), -1);
    assert!(ctx.def.target().is_none());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Definition target missing: target.mty");
}

#[test]
fn rename_prepare_miss_reports_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    ctx.tabs
        .active_model_mut()
        .set_text_preserving_cursor("fn main() {\n  1\n}");
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_rename_prepare(h, 0, 0), 0);
    assert_eq!(crate::mui_rename_active(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No rename target");
}

#[test]
fn explicit_completion_reports_empty_result_only_when_empty() {
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::mui_complete_report_empty(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No completions available");

    ctx.tabs.active_model_mut().set_text_preserving_cursor("alpha al");
    ctx.tabs.active_model_mut().move_to(0, 8);
    assert!(crate::mui_ed_complete_request(h) > 0);
    assert!(crate::mui_complete_count(h) > 0);
    let before = ctx.toasts.toasts().len();
    assert_eq!(crate::mui_complete_report_empty(h), 1);
    assert_eq!(ctx.toasts.toasts().len(), before);
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
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Only one editor pane"
    );
    assert_eq!(mui_pane_close(h), 1);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Only one editor pane"
    );
    assert_eq!(mui_tab_active(h), 0);

    // --- split -> two panes, new (right) pane focused, active tab rebinds --
    assert_eq!(mui_pane_split_right(h), 2);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Split editor right"
    );
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
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Focused editor pane 1"
    );
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
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Closed editor pane"
    );
    assert_eq!(mui_pane_count(h), 1);
    assert_eq!(mui_pane_focused(h), 0);
    // The surviving (left) pane shows tab 0 and is the active tab.
    assert_eq!(mui_pane_tab(h, 0), 0);
    assert_eq!(mui_tab_active(h), 0);

    // --- palette dispatch routes the same as the direct ops ----------------
    assert_eq!(mui_pane_dispatch(h, crate::palette::CMD_SPLIT_RIGHT as i32), 2);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Split editor right"
    );
    assert_eq!(mui_pane_dispatch(h, crate::palette::CMD_CLOSE_PANE as i32), 1);
    assert_eq!(
        ctx.toasts.toasts().last().unwrap().message,
        "Closed editor pane"
    );
    // An out-of-block id is ignored (returns the current count, no panic).
    assert_eq!(mui_pane_dispatch(h, 0), 1);

    let _ = std::fs::remove_file(std::env::temp_dir().join("mui_pane_b.txt"));
}

#[test]
fn markdown_preview_header_close_hit_collapses_preview() {
    use crate::ffi::MuiEvent;

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
    ctx.language = crate::langdetect::Language::Markdown;
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::abi::mui_md_open(h), 1);
    assert_eq!(crate::abi::mui_md_active(h), 1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Markdown preview opened");

    let visible_w = crate::layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width) as f32;
    let region = crate::layout::region(ctx.sidebar_visible);
    let preview_i = ctx.md_pane.expect("preview pane");
    let count = ctx.panes.count();
    let pr = crate::layout::pane_region(region, visible_w, count, preview_i);
    let (_left, x_right) = crate::layout::pane_bounds(region, visible_w, count, preview_i);
    let (x, y, w, hrect) = crate::mdpreview::close_rect(pr, x_right, visible_w);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        x + w * 0.5,
        y + hrect * 0.5,
        0,
    );

    assert_eq!(crate::abi::mui_md_close_at_click(h), 1);
    assert_eq!(crate::abi::mui_md_active(h), 0);
    assert_eq!(crate::abi::mui_pane_count(h), 1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Markdown preview closed");
}

#[test]
fn markdown_breadcrumb_reserves_preview_button_space() {
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 520;
    ctx.gpu.phys_width = 520;
    ctx.gpu.height = 360;
    ctx.gpu.phys_height = 360;
    ctx.sidebar_visible = true;
    ctx.language = crate::langdetect::Language::Markdown;
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(520);

    let left = crate::layout::body_left(ctx.sidebar_visible);
    let (bx, _by, bw, _bh) = crate::abi::md_button_rect(520.0, crate::layout::TAB_BAR_H, crate::layout::BREADCRUMB_H);
    assert!(bx > left, "preview button should remain in the editor breadcrumb band");

    let text_right = bx - 8.0;
    let parent_x = left + 16.0 + 13.0 + 6.0;
    let parent_right = parent_x + (text_right - parent_x) * 0.34;
    let parent = crate::abi::fit_breadcrumb_segment(
        &mut ctx.text,
        "very_long_workspace_name_that_used_to_crowd_the_markdown_preview_button",
        parent_right - parent_x,
        crate::theme::CHROME_FONT_SIZE,
    );
    let (parent_w, _) = ctx.text.measure_ui_sized(&parent, crate::theme::CHROME_FONT_SIZE);
    assert!(
        parent_x + parent_w <= parent_right,
        "workspace segment should be capped before it consumes the file budget: {parent}"
    );

    let file_x = parent_x + parent_w + 20.0;
    let file_right = file_x + (text_right - file_x) * 0.68;
    let shown = crate::abi::fit_breadcrumb_segment(
        &mut ctx.text,
        "feature_walkthrough_with_a_long_markdown_filename_that_used_to_run_under_preview.md",
        file_right - file_x,
        crate::theme::CHROME_FONT_SIZE,
    );
    let (shown_w, _) = ctx.text.measure_ui_sized(&shown, crate::theme::CHROME_FONT_SIZE);

    assert!(
        file_x + shown_w <= file_right,
        "breadcrumb text should stop before Preview pill: shown={shown}"
    );
    assert!(shown.ends_with("md"), "Markdown filename should keep its extension: {shown}");
    assert!(
        bx + bw <= 520.0 - 12.0,
        "preview pill geometry should stay right-aligned and measurable"
    );
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(900);
}

#[test]
fn breadcrumb_accept_misses_report_visible_feedback() {
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::navsurfaces::mui_crumb_menu_accept(h, -1), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No breadcrumb menu open");

    let item = crate::crumbmenu::MenuItem {
        label: "missing.mty".to_string(),
        icon: None,
        icon_color: crate::theme::TEXT(),
        depth: 0,
        target: 0,
    };
    ctx.crumb_menu
        .open(crate::crumbmenu::MenuKind::Files, vec![item.clone()], 80.0);
    assert_eq!(crate::navsurfaces::mui_crumb_menu_accept(h, 3), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No breadcrumb row selected");

    let missing = std::env::temp_dir()
        .join(format!("mui_crumb_missing_{}", std::process::id()))
        .join("missing.mty");
    ctx.crumb_files = vec![missing];
    ctx.crumb_menu
        .open(crate::crumbmenu::MenuKind::Files, vec![item], 80.0);
    assert_eq!(crate::navsurfaces::mui_crumb_menu_accept(h, -1), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Breadcrumb target missing: missing.mty");
}

#[test]
fn problems_header_close_hit_collapses_panel_with_feedback() {
    use crate::ffi::MuiEvent;

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
    ctx.gpu.phys_width = 0;
    ctx.gpu.phys_height = 0;
    ctx.sidebar_visible = false;
    ctx.problems.set_open(true);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let (x, y, w, hrect) = crate::layout::dock_close_rect(ctx.gpu.width, ctx.gpu.height);
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        crate::ffi::MUI_MOUSE_LEFT,
        x + w * 0.5,
        y + hrect * 0.5,
        0,
    );

    assert_eq!(crate::navsurfaces::mui_problems_close_at_click(h), 1);
    assert_eq!(crate::navsurfaces::mui_problems_toggle(h), 0);
    assert_eq!(crate::navsurfaces::mui_problems_is_open(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Problems panel closed");
}

#[test]
fn markdown_preview_rejects_non_markdown_active_file() {
    let mut ctx = ctx_or_skip!();
    ctx.language = crate::langdetect::Language::Mighty;
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::abi::mui_md_open(h), 0);
    assert_eq!(crate::abi::mui_md_active(h), 0);
    assert_eq!(crate::abi::mui_pane_count(h), 1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Markdown Preview is available for Markdown files");
}

#[test]
fn markdown_preview_hides_sidebar_when_compact_and_restores_on_close() {
    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 520;
    ctx.gpu.phys_width = 520;
    ctx.gpu.height = 360;
    ctx.gpu.phys_height = 360;
    ctx.sidebar_visible = true;
    ctx.language = crate::langdetect::Language::Markdown;
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(520);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(crate::abi::mui_md_open(h), 1);
    assert_eq!(crate::abi::mui_md_active(h), 1);
    assert!(!ctx.sidebar_visible, "compact preview should give width back to the panes");
    let visible_w = crate::layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width) as f32;
    let region = crate::layout::region(ctx.sidebar_visible);
    let (left, right) = crate::layout::pane_bounds(region, visible_w, ctx.panes.count(), 1);
    assert!(
        right - left >= 220.0,
        "preview pane should be readable after hiding sidebar"
    );

    crate::abi::mui_md_close(h);
    assert_eq!(crate::abi::mui_md_active(h), 0);
    assert!(ctx.sidebar_visible, "closing preview restores the user's sidebar");
    crate::layout::reset_sidebar_preset();
    crate::layout::set_window_width(900);
}

#[test]
fn minimap_hides_in_narrow_split_panes() {
    assert!(crate::abi::should_show_minimap(
        true,
        false,
        true,
        crate::abi::MINIMAP_MIN_PANE_W
    ));
    assert!(!crate::abi::should_show_minimap(
        true,
        true,
        true,
        crate::abi::MINIMAP_SPLIT_MIN_PANE_W - 1.0
    ));
    assert!(crate::abi::should_show_minimap(
        true,
        true,
        true,
        crate::abi::MINIMAP_SPLIT_MIN_PANE_W
    ));
    assert!(!crate::abi::should_show_minimap(
        true,
        true,
        false,
        crate::abi::MINIMAP_SPLIT_MIN_PANE_W
    ));
    assert!(!crate::abi::should_show_minimap(false, false, true, 800.0));
}

#[test]
fn minimap_autoopen_forces_capture_visibility() {
    let abi = std::fs::read_to_string("src/abi.rs").expect("abi source");
    let start = abi.find("MUI_MINIMAP_AUTOOPEN").expect("minimap autoopen hook");
    let rest = &abi[start..];
    let next = rest
        .find("handle\n}")
        .map(|i| start + i)
        .unwrap_or(abi.len());
    let block = &abi[start..next];
    assert!(
        block.contains("s.minimap = true") && block.contains("ctx.force_minimap_visible = true"),
        "minimap capture must force the setting on so the gallery does not silently show no minimap"
    );
}

#[test]
fn minimap_strip_anchors_to_pane_right_edge() {
    let x_right = 560.0;
    let mm_w = crate::abi::minimap_width_for_pane(280.0);
    let mm_x = x_right - mm_w;
    assert_eq!(mm_w, crate::abi::MINIMAP_COMPACT_W);
    assert_eq!(mm_x, 520.0);
    assert!(mm_x + mm_w <= x_right);
    assert_eq!(crate::abi::minimap_width_for_pane(420.0), crate::abi::MINIMAP_W);
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
fn in_file_replace_reports_noop_and_success_states() {
    use crate::{mui_replace_all, mui_replace_next};

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    *ctx.tabs.active_model_mut() = crate::editor::TextModel::from_bytes(b"alpha beta alpha");

    ctx.replace_bar.open("");
    assert_eq!(mui_replace_all(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Enter text to replace");

    ctx.replace_bar.open("gamma");
    assert_eq!(mui_replace_next(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No matches to replace");

    ctx.replace_bar.open("alpha");
    assert_eq!(ctx.replace_bar.toggle_focus(), 1);
    for c in "omega".chars() {
        ctx.replace_bar.push(c as u32);
    }
    assert_eq!(mui_replace_next(h), 1);
    assert_eq!(ctx.tabs.active_model().line(0), "omega beta alpha");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Replaced 1 occurrence");

    ctx.replace_bar.open("alpha");
    assert_eq!(ctx.replace_bar.toggle_focus(), 1);
    for c in "omega".chars() {
        ctx.replace_bar.push(c as u32);
    }
    assert_eq!(mui_replace_all(h), 1);
    assert_eq!(ctx.tabs.active_model().line(0), "omega beta omega");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Success);
    assert_eq!(toast.message, "Replaced 1 occurrence");
}

#[test]
fn in_file_replace_reports_read_only_preview() {
    use crate::mui_replace_all;

    let mut ctx = ctx_or_skip!();
    let root = std::env::temp_dir().join("mui_replace_read_only_preview");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("asset.bin");
    std::fs::write(&path, b"\0binary foo").unwrap();
    ctx.tabs.open_path(path);
    assert!(ctx.tabs.active_read_only());
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    ctx.replace_bar.open("foo");
    assert_eq!(mui_replace_all(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert_eq!(toast.message, "Replace is unavailable in read-only previews");

    let _ = std::fs::remove_dir_all(root);
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
fn workspace_open_dialog_cancel_does_not_fallback_or_mutate() {
    use crate::wsabi::{mui_ws_open_dialog, mui_ws_recent_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;
    let before_root = ctx.workspace.root().to_path_buf();

    std::env::set_var("MUI_OPEN_FOLDER_PICK", "");
    let opened = mui_ws_open_dialog(h);
    std::env::remove_var("MUI_OPEN_FOLDER_PICK");

    assert_eq!(opened, 0, "cancelled folder picker should be a no-op");
    assert_eq!(ctx.workspace.root(), before_root.as_path());
    assert_eq!(mui_ws_recent_count(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Open folder cancelled");
}

#[test]
fn workspace_open_recent_prunes_missing_folder() {
    use crate::wsabi::{mui_ws_open_recent, mui_ws_recent_count};

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_ws_open_recent(h, -1), 0, "negative recent row should fail");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No recent folder selected");

    assert_eq!(
        mui_ws_open_recent(h, 0),
        0,
        "out-of-range recent row should fail"
    );
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No recent folder selected");

    let missing = std::env::temp_dir().join(format!(
        "mui_ws_recent_missing_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&missing);
    ctx.recent_workspaces.record(missing.clone());
    assert_eq!(mui_ws_recent_count(h), 1);

    assert_eq!(mui_ws_open_recent(h, 0), 0, "missing recent folder should fail");
    assert_eq!(mui_ws_recent_count(h), 0, "stale recent folder should be pruned");
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Warn);
    assert!(toast.message.starts_with("Recent folder missing:"));
}

#[test]
fn open_recent_available_when_only_recent_files_exist() {
    use crate::mui_recent_any;

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_recent_any(h), 0);

    ctx.quickopen.set_recent_paths(vec![std::path::PathBuf::from("main.mty")]);
    assert_eq!(
        mui_recent_any(h),
        1,
        "Open Recent should use the recents picker when only recent files exist"
    );
}

#[test]
fn quickopen_open_prunes_missing_recent_files_before_rendering() {
    use crate::mui_quickopen_open;

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let root = std::env::temp_dir().join(format!("mui_qo_prune_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let keep = root.join("keep.mty");
    let missing = root.join("missing.mty");
    std::fs::write(&keep, b"fn main() {}").unwrap();
    ctx.quickopen.set_recent_paths(vec![missing, keep.clone()]);

    mui_quickopen_open(h);
    assert_eq!(ctx.quickopen.recent_paths(), vec![keep]);
    assert_eq!(ctx.quickopen.count(), 1, "stale recent should not render");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn welcome_open_recent_misses_report_visible_feedback() {
    use crate::mui_welcome_open_recent;

    let mut ctx = ctx_or_skip!();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    assert_eq!(mui_welcome_open_recent(h, -1), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No recent file selected");

    assert_eq!(mui_welcome_open_recent(h, 0), -1);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No recent file selected");
}

#[test]
fn welcome_missing_recent_folder_stays_open_and_prunes() {
    use crate::wsabi::mui_ws_recent_count;
    use crate::{mui_welcome_active, mui_welcome_draw, mui_welcome_open_folder};

    let mut ctx = ctx_or_skip!();
    ctx.gpu.width = 900;
    ctx.gpu.height = 700;
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let root = std::env::temp_dir().join(format!("mui_welcome_folder_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("main.mty");
    std::fs::write(&file, b"fn main() {}").unwrap();
    ctx.tabs.open_path(file);

    let missing = root.join("missing-folder");
    let _ = std::fs::remove_dir_all(&missing);
    ctx.recent_workspaces.set_all(vec![missing]);
    ctx.welcome.open();

    assert_eq!(mui_welcome_open_folder(h, -1), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No recent folder selected");

    assert_eq!(mui_welcome_open_folder(h, 0), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "No recent folder selected");

    mui_welcome_draw(h);
    assert_eq!(
        mui_ws_recent_count(h),
        0,
        "stale Welcome folder should be pruned before rendering"
    );
    assert_eq!(
        mui_welcome_open_folder(h, 0),
        0,
        "missing Welcome recent folder should fail"
    );
    assert_eq!(mui_ws_recent_count(h), 0, "stale Welcome folder should be pruned");
    assert_eq!(
        mui_welcome_active(h),
        1,
        "failed Welcome recent-folder open should not dismiss the forced Welcome screen"
    );

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
fn open_file_dialog_sequence_picks_distinct_files() {
    use crate::{mui_open_file_dialog, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let root = std::env::temp_dir().join(format!("mui_open_file_sequence_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let first = root.join("first.mty");
    let second = root.join("second.md");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"# Second").unwrap();
    let seq = format!("{}|{}", first.display(), second.display());

    std::env::remove_var("MUI_OPEN_FILE_PICK");
    std::env::set_var("MUI_OPEN_FILE_PICK_SEQUENCE", seq);
    let first_idx = mui_open_file_dialog(h);
    let second_idx = mui_open_file_dialog(h);
    std::env::remove_var("MUI_OPEN_FILE_PICK_SEQUENCE");

    assert_eq!(first_idx, 1);
    assert_eq!(second_idx, 2);
    assert_eq!(mui_tab_count(h), 3);
    assert_eq!(mui_tab_active(h), second_idx);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(second.as_path()));
    assert_eq!(ctx.tabs.active_model().as_text(), "# Second");
    assert_eq!(ctx.language, crate::langdetect::Language::Markdown);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_file_dialog_cancel_does_not_open_prompt_signal() {
    use crate::{mui_open_file_dialog, mui_tab_active, mui_tab_count};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_OPEN_FILE_PICK", "");
    let idx = mui_open_file_dialog(h);
    std::env::remove_var("MUI_OPEN_FILE_PICK");

    assert_eq!(idx, -2, "cancelled file picker should not request prompt fallback");
    assert_eq!(mui_tab_count(h), 1);
    assert_eq!(mui_tab_active(h), 0);
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Open file cancelled");
}

#[test]
fn native_file_dialogs_start_in_active_file_folder() {
    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();

    let root = std::env::temp_dir().join(format!("mui_dialog_initial_dir_{}", std::process::id()));
    let nested = root.join("src").join("feature");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    ctx.tree.set_root(root.clone());

    assert_eq!(
        crate::abi::file_dialog_initial_dir(&ctx),
        root,
        "untitled tabs should fall back to the workspace root"
    );

    let active = nested.join("main.mty");
    std::fs::write(&active, b"fn main() {}").unwrap();
    ctx.tabs.open_path(active);
    assert_eq!(
        crate::abi::file_dialog_initial_dir(&ctx),
        nested,
        "file dialogs should open beside the active file"
    );

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
fn plain_save_on_untitled_uses_native_save_picker() {
    use crate::{mui_active_has_path, mui_ed_dirty, mui_ed_save};

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

    let root = std::env::temp_dir().join(format!("mui_plain_save_untitled_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let target = root.join("saved.mty");

    std::env::set_var("MUI_SAVE_FILE_PICK", target.to_string_lossy().as_ref());
    let saved = mui_ed_save(h);
    std::env::remove_var("MUI_SAVE_FILE_PICK");

    assert_eq!(saved, 0);
    assert_eq!(mui_active_has_path(h), 1);
    assert_eq!(mui_ed_dirty(h), 0);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(target.as_path()));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {\n");

    crate::settings::set_active(before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn save_as_dialog_cancel_leaves_untitled_dirty() {
    use crate::{mui_active_has_path, mui_ed_dirty, mui_save_as_dialog};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::settings::active();
    crate::settings::set_active(crate::settings::Settings::default());

    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("fn main() {}");
    ctx.tabs.set_dirty(ctx.tabs.active(), true);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_SAVE_FILE_PICK", "");
    let saved = mui_save_as_dialog(h);
    std::env::remove_var("MUI_SAVE_FILE_PICK");

    assert_eq!(saved, -2, "cancelled Save As should not request prompt fallback");
    assert_eq!(mui_active_has_path(h), 0);
    assert_eq!(mui_ed_dirty(h), 1);
    assert!(ctx.tabs.active_path().is_none());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Save cancelled; tab is still open");

    crate::settings::set_active(before);
}

#[test]
fn plain_save_on_untitled_cancel_keeps_dirty_and_toasts() {
    use crate::{mui_active_has_path, mui_ed_dirty, mui_ed_save};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::settings::active();
    crate::settings::set_active(crate::settings::Settings::default());

    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("fn main() {}");
    ctx.tabs.set_dirty(ctx.tabs.active(), true);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    std::env::set_var("MUI_SAVE_FILE_PICK", "");
    let saved = mui_ed_save(h);
    std::env::remove_var("MUI_SAVE_FILE_PICK");

    assert_eq!(saved, -2, "cancelled Save should keep the untitled tab open");
    assert_eq!(mui_active_has_path(h), 0);
    assert_eq!(mui_ed_dirty(h), 1);
    assert!(ctx.tabs.active_path().is_none());
    let toast = ctx.toasts.toasts().last().unwrap();
    assert_eq!(toast.kind, crate::toast::Kind::Info);
    assert_eq!(toast.message, "Save cancelled; tab is still open");

    crate::settings::set_active(before);
}

#[test]
fn save_as_prompt_consumes_staged_path() {
    use crate::{mui_active_has_path, mui_ed_dirty, mui_path_push, mui_save_as};

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::settings::active();
    crate::settings::set_active(crate::settings::Settings::default());

    let mut ctx = ctx_or_skip!();
    ctx.tabs.ensure_scratch();
    ctx.tabs.active_model_mut().set_text_preserving_cursor("let x = 1");
    ctx.tabs.set_dirty(ctx.tabs.active(), true);
    let h = (&mut ctx as *mut MuiContext) as usize as i64;

    let root = std::env::temp_dir().join(format!("mui_save_as_prompt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let target = root.join("typed.mty");
    for b in target.to_string_lossy().as_bytes() {
        mui_path_push(h, *b as u32);
    }

    assert_eq!(mui_save_as(h), 0);
    assert!(ctx.path_stage.is_empty());
    assert_eq!(mui_active_has_path(h), 1);
    assert_eq!(mui_ed_dirty(h), 0);
    assert_eq!(ctx.tabs.active_path().as_deref(), Some(target.as_path()));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "let x = 1\n");

    crate::settings::set_active(before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lightbulb_visibility_and_click_open_actions() {
    use crate::ffi::MuiEvent;
    use crate::wsabi::{
        mui_lightbulb_click, mui_lightbulb_line, mui_lightbulb_reset, mui_lightbulb_visible,
    };

    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let zen_before = crate::layout::zen_active();
    crate::layout::set_zen(false);
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
    crate::layout::set_zen(zen_before);
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

#[test]
fn cursor_move_translates_to_mouse_move_event() {
    let _g = crate::settings::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let os_scale = crate::uiscale::os_scale();
    let user_zoom = crate::uiscale::user_zoom();
    crate::uiscale::set_os_scale(1.0);
    crate::uiscale::set_user_zoom(1.0);
    let mut q = EventQueue::default();
    translate_window_event(
        &mut q,
        &winit::event::WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        },
    );
    translate_window_event(
        &mut q,
        &winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(123.0, 456.0),
        },
    );
    assert_eq!(q.pop().unwrap().tag, MUI_EVENT_MOUSE_DOWN);
    let ev = q.pop().unwrap();
    assert_eq!(ev.tag, MUI_EVENT_MOUSE_MOVE);
    assert_eq!(ev.x, 123.0);
    assert_eq!(ev.y, 456.0);
    crate::uiscale::set_os_scale(os_scale);
    crate::uiscale::set_user_zoom(user_zoom);
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
    assert!(
        main.contains("topbar_act == 3") && main.contains("mui_quickopen_open(h)"),
        "titlebar command center must route to Quick Open"
    );
    assert!(
        main.contains("topbar_early == 1")
            && main.contains("let opened = mui_run_toggle(h)")
            && main.contains("if mui_run_running(h) == 0 { let _r = mui_run_start(h) }"),
        "the early topbar guard must run the visible play button instead of swallowing it"
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
    assert!(
        main.contains("Ctrl+S save / Ctrl+Shift+S Save As"),
        "main editor key routing must keep Ctrl+Shift+S on the Save As path"
    );
    assert!(
        main.contains("let chrome_click_allowed =")
            && main.contains("prompt_kind == 0 && !palette_open && !quickopen_open && !settings_open && !theme_picker_open")
            && main.contains("chrome_click_allowed && mui_bottom_dock_resize_at_click(h) == 1")
            && main.contains("chrome_click_allowed && mui_sidebar_resize_at_click(h) == 1"),
        "manual resize/header controls must not steal clicks while prompts or modal overlays are open"
    );
    assert!(
        main.contains("if shift_held(mods) {\n            let sr = mui_save_as_dialog(h)"),
        "Ctrl+Shift+S should force the native Save As dialog even for file-backed tabs"
    );
    assert!(
        main.contains("if mui_recent_any(h) == 1"),
        "File: Open Recent must open the recents picker when either recent files or folders exist"
    );
    assert!(
        main.contains("mui_welcome_open_recent_picker(h)"),
        "File: Open Recent should use the focused recent picker, not the branded Welcome landing"
    );
    assert!(
        main.contains("let np = mui_newproj_dialog(h)")
            && main.contains("if np == -1 {\n              mui_prompt_open(h, prompt_new_project())"),
        "New Project should use the native project-folder picker before falling back to the bottom prompt"
    );
    assert!(
        main.contains("fn welcome_close() -> I32 { 9 }")
            && main.contains("welcome_act == welcome_close()")
            && main.contains("mui_welcome_dismiss(h)"),
        "Open Recent picker close button must dismiss the forced Welcome surface"
    );
    assert!(
        main.contains("key_page_up()") && main.contains("mui_tab_move_active_left(h)"),
        "Ctrl+Shift+PageUp should route to Move Active Tab Left"
    );
    assert!(
        main.contains("key_page_down()") && main.contains("mui_tab_move_active_right(h)"),
        "Ctrl+Shift+PageDown should route to Move Active Tab Right"
    );
    assert!(
        main.contains("id == cmd_peek_close()") && main.contains("mui_peek_close(h)"),
        "Peek: Close View must reuse the same close path as Esc"
    );
    assert!(
        main.contains("id == cmd_explorer_collapse_all()")
            && main.contains("let _vp = mui_panel_set(h, panel_explorer())")
            && main.contains("mui_tree_collapse_all(h)"),
        "Explorer collapse-all command must reveal Explorer before collapsing the tree"
    );
    assert!(
        main.contains("id == cmd_explorer_refresh()")
            && main.contains("let _vp = mui_panel_set(h, panel_explorer())")
            && main.contains("let _tr = mui_tree_refresh(h)")
            && main.contains("let _qr = mui_quickopen_reindex(h)"),
        "Explorer refresh command must reveal Explorer and refresh both tree and Quick Open index"
    );
    assert!(
        main.contains("id == cmd_git_refresh_source_control()")
            && main.contains("let _vp = mui_panel_set(h, panel_scm())")
            && main.contains("let _r = mui_scm_refresh(h)"),
        "Git refresh command must reveal Source Control before refreshing status"
    );
    assert!(
        main.contains("id == cmd_problems_refresh()")
            && main.contains("let _dr = mui_diag_refresh(h)")
            && main.contains("let _pr = mui_problems_refresh(h)")
            && main.contains("let _po = mui_problems_open(h)"),
        "Problems refresh command must refresh diagnostics, aggregate Problems, and show the panel"
    );
    assert!(
        main.contains("id == cmd_outline_refresh()")
            && main.contains("let _vp = mui_panel_set(h, panel_outline())")
            && main.contains("let _or = mui_outline_refresh(h)"),
        "Outline refresh command must reveal Outline before refreshing symbols"
    );
    assert!(
        main.contains("id == cmd_agents_refresh()")
            && main.contains("let _p = mui_panel_set(h, panel_agents_mty())")
            && main.contains("let _a = mui_agents_refresh(h)"),
        "Agents refresh command must reveal Mighty Agents before refreshing topology"
    );
    assert!(
        main.contains("id == cmd_agents_clear_run_output()")
            && main.contains("let _p = mui_panel_set(h, panel_agents_mty())")
            && main.contains("let _ac = mui_agents_clear_run_output(h)")
            && main.contains("agents_focus = true"),
        "Agents clear-run-output command must reveal Mighty Agents before clearing its run transcript"
    );
    assert!(
        main.contains("id == cmd_run_clear_output()")
            && main.contains("let _rc = mui_run_clear(h)")
            && main.contains("run_focus = true"),
        "Run clear-output command must reveal Run before clearing rendered output"
    );
    assert!(
        main.contains("id == cmd_test_clear_results()")
            && main.contains("let _tc = mui_test_clear(h)")
            && main.contains("test_focus = true"),
        "Test clear-results command must reveal Testing before clearing parsed results"
    );
    assert!(
        main.contains("id == cmd_web_clear_output()")
            && main.contains("let _wc = mui_web_clear(h)")
            && main.contains("web_focus = true"),
        "Web clear-output command must reveal Web Playground before clearing output"
    );
    assert!(
        main.contains("id == cmd_diff_close_view()")
            && main.contains("mui_diff_close(h)")
            && main.contains("diff_open = false"),
        "Diff close command must close the shim diff view and clear Mighty's diff-open flag"
    );
    assert!(
        main.contains("id == cmd_git_hide_blame()")
            && main.contains("let _bc = mui_blame_close(h)"),
        "Git hide-blame command must call the dedicated blame close ABI"
    );
    for (helper, action) in [
        ("cmd_search_run", "mui_search_run(h)"),
        ("cmd_search_replace_all", "mui_search_replace_all(h)"),
        ("cmd_search_toggle_replace", "mui_search_toggle_focus(h)"),
    ] {
        assert!(
            main.contains(&format!("id == {helper}()"))
                && main.contains("let _vp = mui_panel_set(h, panel_search())")
                && main.contains(action),
            "Search command `{helper}` must reveal Search before invoking `{action}`"
        );
    }
    for needle in [
        "id >= cmd_pane_first() && id <= cmd_pane_last()",
        "id >= cmd_git_first() && id <= cmd_git_last()",
        "id >= cmd_ws_first() && id <= cmd_ws_last()",
        "id >= cmd_fold_first() && id <= cmd_fold_last()",
        "id >= cmd_sidebar_layout_first() && id <= cmd_sidebar_layout_last()",
        "id == cmd_keyboard_shortcuts()",
        "id == cmd_clear_notifications()",
        "id == cmd_explorer_collapse_all()",
        "id == cmd_new_project()",
        "id == cmd_new_workspace_file()",
        "mui_prompt_open(h, prompt_new_file())",
    ] {
        assert!(
            main.contains(needle),
            "central command dispatcher must cover `{needle}` so palette/quick-open rows do not become inert"
        );
    }
}

#[test]
fn screenshot_autoopen_diff_dismisses_welcome_overlay() {
    let abi = include_str!("abi.rs");
    let marker = "MUI_DIFF_AUTOOPEN";
    let start = abi.find(marker).expect("diff autoopen hook should exist");
    let next = abi[start..]
        .find("MUI_MD_AUTOOPEN")
        .map(|i| start + i)
        .unwrap_or(abi.len());
    let block = &abi[start..next];
    assert!(block.contains("ctx.diff.open"), "diff autoopen hook should open the diff view");
    assert!(
        block.contains("ctx.welcome.dismiss_empty_auto()"),
        "diff autoopen hook must suppress automatic empty-buffer Welcome so captures show the diff body"
    );
}

#[test]
fn language_feature_autoopen_captures_dismiss_welcome_overlay() {
    let abi = include_str!("abi.rs");
    for (marker, next_marker, target) in [
        (
            "if std::env::var_os(\"MUI_RENAME_AUTOOPEN\").is_some()",
            "if std::env::var_os(\"MUI_CODEACTION_AUTOOPEN\").is_some()",
            "ctx.rename.open",
        ),
        (
            "if let Some(seed) = std::env::var_os(\"MUI_GHOST_AUTOOPEN\")",
            "// Screenshot/render hook for the activity-rail panels",
            "ctx.ghost.seed_demo",
        ),
    ] {
        let start = abi.find(marker).expect("language feature autoopen hook should exist");
        let next = abi[start + marker.len()..]
            .find(next_marker)
            .map(|i| start + marker.len() + i)
            .unwrap_or(abi.len());
        let block = &abi[start..next];
        assert!(
            block.contains("TextModel::from_bytes"),
            "{marker} hook must seed code so the capture proves the editor surface"
        );
        assert!(block.contains(target), "{marker} hook must open the target feature");
        assert!(
            block.contains("ctx.welcome.dismiss()"),
            "{marker} hook must dismiss Welcome so captures show the feature over code"
        );
        assert!(
            block.contains("ctx.edit_probe_lock = true"),
            "{marker} hook must keep the seeded code stable during screenshot capture"
        );
    }
}

#[test]
fn shortcuts_autoopen_uses_single_mighty_draw_path() {
    let lib = include_str!("lib.rs");
    let abi = include_str!("abi.rs");
    let draw_start = abi.find("pub extern \"C\" fn mui_keys_draw").unwrap();
    let draw_end = abi[draw_start..]
        .find("// ---------------------------------------------------------------------------\n// Color theme")
        .map(|i| draw_start + i)
        .unwrap_or(abi.len());
    let draw_block = &abi[draw_start..draw_end];

    assert!(
        abi.contains("ctx.shortcuts.open()") && abi.contains("ctx.shortcuts_autoopen = true"),
        "shortcuts auto-open hook should activate the engine for the Mighty frame draw"
    );
    assert!(
        !lib.contains("ctx.shortcuts_autoopen && ctx.shortcuts.is_active()"),
        "shortcuts must not be force-drawn again in end_frame; double drawing creates offset overlay cards"
    );
    assert!(
        draw_block.contains("visible_surface_size(ctx)") && !draw_block.contains("ctx.gpu.width, ctx.gpu.height"),
        "shortcuts overlay geometry must honor screenshot/window visible bounds, not raw GPU dimensions"
    );
}

#[test]
fn every_palette_command_is_routed_by_mighty_dispatcher() {
    use crate::palette::*;

    let main = include_str!("../../../src/main.mty");
    let ranges = [
        (CMD_GIT_FIRST, CMD_GIT_TOGGLE_BLAME),
        (CMD_PANE_FIRST, CMD_PANE_LAST),
        (CMD_WS_FIRST, CMD_WS_LAST),
        (CMD_FOLD_FIRST, CMD_FOLD_LAST),
        (CMD_DOCK_FIRST, CMD_DOCK_LAST),
        (CMD_SIDEBAR_FIRST, CMD_SIDEBAR_LAST),
    ];
    let direct = [
        (CMD_NEW_FILE, "cmd_new_file"),
        (CMD_NEW_WORKSPACE_FILE, "cmd_new_workspace_file"),
        (CMD_NEW_UNTITLED_FILE, "cmd_new_untitled_file"),
        (CMD_NEW_FOLDER, "cmd_new_folder"),
        (CMD_RENAME_ACTIVE_FILE, "cmd_rename_active_file"),
        (CMD_REVEAL_ACTIVE_FILE, "cmd_reveal_active_file"),
        (CMD_EXPLORER_REFRESH, "cmd_explorer_refresh"),
        (CMD_EXPLORER_COLLAPSE_ALL, "cmd_explorer_collapse_all"),
        (CMD_DELETE_ACTIVE_FILE, "cmd_delete_active_file"),
        (CMD_REVEAL_ACTIVE_FILE_IN_OS, "cmd_reveal_active_file_in_os"),
        (CMD_COPY_ACTIVE_FILE_PATH, "cmd_copy_active_file_path"),
        (
            CMD_COPY_ACTIVE_FILE_RELATIVE_PATH,
            "cmd_copy_active_file_relative_path",
        ),
        (CMD_COPY_ACTIVE_FILE_NAME, "cmd_copy_active_file_name"),
        (
            CMD_COPY_ACTIVE_FILE_DIRECTORY,
            "cmd_copy_active_file_directory",
        ),
        (CMD_SELECT_ALL, "cmd_select_all"),
        (CMD_SELECT_LINE, "cmd_select_line"),
        (CMD_SELECT_WORD, "cmd_select_word"),
        (CMD_TOGGLE_LINE_COMMENT, "cmd_toggle_line_comment"),
        (CMD_COPY_SELECTION_OR_LINE, "cmd_copy_selection_or_line"),
        (CMD_CUT_SELECTION_OR_LINE, "cmd_cut_selection_or_line"),
        (CMD_PASTE_IN_EDITOR, "cmd_paste_in_editor"),
        (CMD_DELETE_PREVIOUS_WORD, "cmd_delete_previous_word"),
        (CMD_DELETE_NEXT_WORD, "cmd_delete_next_word"),
        (CMD_INDENT_LINE_SELECTION, "cmd_indent_line_selection"),
        (CMD_OUTDENT_LINE_SELECTION, "cmd_outdent_line_selection"),
        (CMD_MOVE_WORD_LEFT, "cmd_move_word_left"),
        (CMD_MOVE_WORD_RIGHT, "cmd_move_word_right"),
        (CMD_MOVE_DOCUMENT_START, "cmd_move_document_start"),
        (CMD_MOVE_DOCUMENT_END, "cmd_move_document_end"),
        (CMD_MOVE_LINE_START, "cmd_move_line_start"),
        (CMD_MOVE_LINE_END, "cmd_move_line_end"),
        (
            CMD_ADD_CARET_NEXT_OCCURRENCE,
            "cmd_add_caret_next_occurrence",
        ),
        (CMD_ADD_CARET_ABOVE, "cmd_add_caret_above"),
        (CMD_ADD_CARET_BELOW, "cmd_add_caret_below"),
        (CMD_COLLAPSE_CARETS, "cmd_collapse_carets"),
        (
            CMD_DUPLICATE_LINE_SELECTION,
            "cmd_duplicate_line_selection",
        ),
        (CMD_MOVE_LINE_UP, "cmd_move_line_up"),
        (CMD_MOVE_LINE_DOWN, "cmd_move_line_down"),
        (CMD_DELETE_LINE, "cmd_delete_line"),
        (CMD_JOIN_LINE, "cmd_join_line"),
        (CMD_CLEAR_NOTIFICATIONS, "cmd_clear_notifications"),
        (CMD_OPEN_FILE, "cmd_open_file"),
        (CMD_SAVE, "cmd_save"),
        (CMD_SAVE_AS, "cmd_save_as"),
        (CMD_SAVE_ALL, "cmd_save_all"),
        (CMD_QUICK_OPEN, "cmd_quick_open"),
        (CMD_FIND, "cmd_find"),
        (CMD_FIND_REPLACE, "cmd_find_replace"),
        (CMD_GOTO_LINE, "cmd_goto_line"),
        (CMD_GOTO_DEFINITION, "cmd_goto_definition"),
        (CMD_HOVER, "cmd_hover"),
        (CMD_SIGNATURE_HELP, "cmd_signature_help"),
        (CMD_RENAME_SYMBOL, "cmd_rename_symbol"),
        (CMD_CODE_ACTIONS, "cmd_code_actions"),
        (CMD_TOGGLE_TERMINAL, "cmd_toggle_terminal"),
        (CMD_TOGGLE_SIDEBAR, "cmd_toggle_sidebar"),
        (CMD_NEXT_TAB, "cmd_next_tab"),
        (CMD_PREV_TAB, "cmd_prev_tab"),
        (CMD_CLOSE_TAB, "cmd_close_tab"),
        (CMD_CLOSE_SAVED_TABS, "cmd_close_saved_tabs"),
        (CMD_CLOSE_OTHER_SAVED_TABS, "cmd_close_other_saved_tabs"),
        (CMD_CLOSE_SAVED_TABS_TO_RIGHT, "cmd_close_saved_tabs_to_right"),
        (CMD_CLOSE_SAVED_TABS_TO_LEFT, "cmd_close_saved_tabs_to_left"),
        (CMD_REOPEN_CLOSED_TAB, "cmd_reopen_closed_tab"),
        (CMD_DUPLICATE_ACTIVE_TAB, "cmd_duplicate_active_tab"),
        (CMD_MOVE_ACTIVE_TAB_LEFT, "cmd_move_active_tab_left"),
        (CMD_MOVE_ACTIVE_TAB_RIGHT, "cmd_move_active_tab_right"),
        (CMD_SORT_TABS_BY_NAME, "cmd_sort_tabs_by_name"),
        (CMD_CLOSE_DUPLICATE_TABS, "cmd_close_duplicate_tabs"),
        (CMD_GIT_STAGE_ALL, "cmd_git_stage_all"),
        (CMD_GIT_UNSTAGE_ALL, "cmd_git_unstage_all"),
        (CMD_GIT_COMMIT_STAGED, "cmd_git_commit_staged"),
        (
            CMD_GIT_REFRESH_SOURCE_CONTROL,
            "cmd_git_refresh_source_control",
        ),
        (CMD_VIEW_EXPLORER, "cmd_view_explorer"),
        (CMD_VIEW_SEARCH, "cmd_view_search"),
        (CMD_SEARCH_RUN, "cmd_search_run"),
        (CMD_SEARCH_REPLACE_ALL, "cmd_search_replace_all"),
        (
            CMD_SEARCH_TOGGLE_REPLACE,
            "cmd_search_toggle_replace",
        ),
        (CMD_VIEW_SOURCE_CONTROL, "cmd_view_source_control"),
        (CMD_VIEW_OUTLINE, "cmd_view_outline"),
        (CMD_OUTLINE_REFRESH, "cmd_outline_refresh"),
        (CMD_VIEW_RUN_DEBUG, "cmd_view_run_debug"),
        (CMD_VIEW_TESTING, "cmd_view_testing"),
        (CMD_VIEW_RUN_OUTPUT, "cmd_view_run_output"),
        (CMD_VIEW_PROBLEMS, "cmd_view_problems"),
        (CMD_PROBLEMS_REFRESH, "cmd_problems_refresh"),
        (CMD_VIEW_AI_COPILOT, "cmd_view_ai_copilot"),
        (CMD_INLINE_AI_ASK, "cmd_inline_ai_ask"),
        (
            CMD_FORCE_GHOST_COMPLETION,
            "cmd_force_ghost_completion",
        ),
        (CMD_AI_CLEAR_CHAT, "cmd_ai_clear_chat"),
        (CMD_VIEW_TERMINAL, "cmd_view_terminal"),
        (CMD_VIEW_WEB_PLAYGROUND, "cmd_view_web_playground"),
        (CMD_DEBUG_START_CONTINUE, "cmd_debug_start_continue"),
        (CMD_DEBUG_STOP, "cmd_debug_stop"),
        (CMD_DEBUG_STEP_OVER, "cmd_debug_step_over"),
        (CMD_DEBUG_STEP_INTO, "cmd_debug_step_into"),
        (CMD_DEBUG_STEP_OUT, "cmd_debug_step_out"),
        (CMD_DEBUG_PAUSE, "cmd_debug_pause"),
        (CMD_DEBUG_RESTART, "cmd_debug_restart"),
        (CMD_RELOAD_ACTIVE_FILE, "cmd_reload_active_file"),
        (CMD_REVERT_ACTIVE_FILE, "cmd_revert_active_file"),
        (CMD_FORMAT_DOCUMENT, "cmd_format_document"),
        (CMD_UNDO, "cmd_undo"),
        (CMD_REDO, "cmd_redo"),
        (CMD_AUTOCOMPLETE, "cmd_autocomplete"),
        (CMD_JUMP_BACK, "cmd_jump_back"),
        (CMD_QUIT, "cmd_quit"),
        (CMD_COLOR_THEME, "cmd_color_theme"),
        (CMD_RUN_FILE, "cmd_run_file"),
        (CMD_RUN_STOP, "cmd_run_stop"),
        (CMD_RUN_CLEAR_OUTPUT, "cmd_run_clear_output"),
        (CMD_SETTINGS, "cmd_settings"),
        (CMD_ZOOM_IN, "cmd_zoom_in"),
        (CMD_ZOOM_OUT, "cmd_zoom_out"),
        (CMD_ZOOM_RESET, "cmd_zoom_reset"),
        (CMD_RUN_TESTS, "cmd_run_tests"),
        (CMD_RUN_TEST_AT_CURSOR, "cmd_run_test_at_cursor"),
        (CMD_TEST_STOP, "cmd_test_stop"),
        (CMD_TEST_CLEAR_RESULTS, "cmd_test_clear_results"),
        (CMD_PEEK_DEFINITION, "cmd_peek_definition"),
        (CMD_PEEK_CLOSE, "cmd_peek_close"),
        (CMD_WELCOME, "cmd_welcome"),
        (CMD_ZEN_MODE, "cmd_zen_mode"),
        (CMD_AGENTS, "cmd_agents"),
        (CMD_AGENTS_REFRESH, "cmd_agents_refresh"),
        (
            CMD_AGENTS_CLEAR_RUN_OUTPUT,
            "cmd_agents_clear_run_output",
        ),
        (CMD_RUN_IN_BROWSER, "cmd_run_in_browser"),
        (CMD_WEB_STOP, "cmd_web_stop"),
        (CMD_WEB_OPEN_BROWSER, "cmd_web_open_browser"),
        (CMD_WEB_CLEAR_OUTPUT, "cmd_web_clear_output"),
        (CMD_DIFF_CLOSE_VIEW, "cmd_diff_close_view"),
        (CMD_GIT_HIDE_BLAME, "cmd_git_hide_blame"),
        (CMD_KEYBOARD_SHORTCUTS, "cmd_keyboard_shortcuts"),
        (CMD_NEW_PROJECT, "cmd_new_project"),
        (CMD_WINDOW_TOGGLE_MAXIMIZE, "cmd_window_toggle_maximize"),
        (CMD_WINDOW_MINIMIZE, "cmd_window_minimize"),
        (CMD_DOCK_CLOSE, "cmd_dock_close"),
        (CMD_AI_CLOSE, "cmd_ai_close"),
        (CMD_SIDEBAR_CLOSE, "cmd_sidebar_close"),
        (CMD_SIDEBAR_CYCLE_WIDTH, "cmd_sidebar_cycle_width"),
    ];

    for cmd in COMMANDS {
        if ranges
            .iter()
            .find(|(first, last)| cmd.id >= *first && cmd.id <= *last)
            .is_some()
        {
            continue;
        }
        let Some((_, helper)) = direct.iter().find(|(id, _)| *id == cmd.id) else {
            panic!(
                "palette command `{}` ({}) has no expected dispatcher mapping; add a direct helper or a range",
                cmd.label, cmd.id
            );
        };
        let helper_def = format!("fn {helper}() -> I32 {{ {}", cmd.id);
        assert!(
            main.contains(&helper_def),
            "Mighty helper `{helper}` must mirror palette id {} for `{}`",
            cmd.id,
            cmd.label
        );
        let dispatch_arm = format!("id == {helper}()");
        assert!(
            main.contains(&dispatch_arm),
            "palette command `{}` ({}) must be handled by the central Mighty dispatcher",
            cmd.label,
            cmd.id
        );
    }
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
    use std::sync::MutexGuard;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};

    // Big enough that the title-bar controls (right edge) and interior are
    // distinct, and edges aren't the whole window.
    const WW: u32 = 1000;
    const WH: u32 = 700;

    fn handle(ctx: &mut MuiContext) -> i64 {
        (ctx as *mut MuiContext) as usize as i64
    }

    struct ChromeGlobals {
        _guard: MutexGuard<'static, ()>,
        os_scale: f32,
        user_zoom: f32,
        zen: bool,
    }

    impl ChromeGlobals {
        fn pin() -> Self {
            let guard = crate::settings::TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let os_scale = crate::uiscale::os_scale();
            let user_zoom = crate::uiscale::user_zoom();
            let zen = crate::layout::zen_active();
            crate::uiscale::set_os_scale(1.0);
            crate::uiscale::set_user_zoom(1.0);
            crate::layout::set_zen(false);
            Self {
                _guard: guard,
                os_scale,
                user_zoom,
                zen,
            }
        }
    }

    impl Drop for ChromeGlobals {
        fn drop(&mut self) {
            crate::uiscale::set_os_scale(self.os_scale);
            crate::uiscale::set_user_zoom(self.user_zoom);
            crate::layout::set_zen(self.zen);
        }
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
        let _globals = ChromeGlobals::pin();
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
        let _globals = ChromeGlobals::pin();
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        let h = handle(&mut ctx);
        // Minimize button (leftmost of the three controls).
        let min_x = crate::titlebar::controls_x(WW as f32) + crate::titlebar::BTN_W * 0.5;
        move_to(&mut ctx, min_x, 8.0);
        press_left(&mut ctx);
        // Caption-strip drag region after the visible tabs but before run/more controls.
        let drag_x = crate::titlebar::controls_x(WW as f32) - crate::titlebar::ACTION_STRIP_W - 10.0;
        move_to(&mut ctx, drag_x, 8.0);
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
    fn tab_bar_press_passes_through_to_the_ide() {
        let _globals = ChromeGlobals::pin();
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        ctx.tabs.ensure_scratch();
        let h = handle(&mut ctx);
        let x = crate::layout::body_left(ctx.sidebar_visible) + 24.0;
        move_to(&mut ctx, x, 8.0);
        press_left(&mut ctx);
        assert_eq!(
            mui_poll_event_s(h),
            MUI_EVENT_MOUSE_DOWN as i32,
            "visible tab-bar clicks must reach Mighty instead of starting window drag"
        );
    }

    #[test]
    fn ai_header_close_press_passes_through_to_the_ide() {
        let _globals = ChromeGlobals::pin();
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        ctx.ai.open = true;
        let h = handle(&mut ctx);
        let (x, y, w, hgt) = crate::ai::close_geometry(WW);
        move_to(&mut ctx, x + w * 0.5, y + hgt * 0.5);
        press_left(&mut ctx);
        assert_eq!(
            mui_poll_event_s(h),
            MUI_EVENT_MOUSE_DOWN as i32,
            "AI header close must reach Mighty instead of starting window drag"
        );
    }

    #[test]
    fn interior_press_passes_through_to_the_ide() {
        let _globals = ChromeGlobals::pin();
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
        let _globals = ChromeGlobals::pin();
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
    }

    #[test]
    fn ctrl_wheel_zooms_plain_wheel_scrolls() {
        let _globals = ChromeGlobals::pin();
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
    }

    #[test]
    fn plain_wheel_over_tab_strip_scrolls_overflow_tabs() {
        let _globals = ChromeGlobals::pin();
        let mut ctx = match MuiContext::new_offscreen(WW, WH) {
            Some(c) => c,
            None => return,
        };
        ctx.sidebar_visible = false;
        ctx.tabs.ensure_scratch();
        let root = std::env::temp_dir().join(format!("mui_tab_wheel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..8 {
            let p = root.join(format!("wheel-{i}.mty"));
            std::fs::write(&p, format!("fn wheel_{i}() {{}}\n")).unwrap();
            ctx.tabs.open_path(p);
        }
        ctx.tab_scroll = 0;
        let h = handle(&mut ctx);
        let mods = winit::keyboard::ModifiersState::empty();
        translate_window_event(&mut ctx.queue, &WindowEvent::ModifiersChanged(mods.into()));
        let x = crate::layout::body_left(ctx.sidebar_visible) + 24.0;
        move_to(&mut ctx, x, 8.0);
        wheel(&mut ctx, -1.0);
        assert_eq!(
            mui_poll_event_s(h),
            0,
            "wheel over tab strip should be consumed by tab overflow navigation"
        );
        assert_eq!(ctx.tab_scroll, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_char_without_ctrl_reaches_the_ide() {
        let _globals = ChromeGlobals::pin();
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
