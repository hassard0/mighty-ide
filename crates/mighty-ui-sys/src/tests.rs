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
    use crate::{
        mui_path_push, mui_tab_active, mui_tab_close, mui_tab_count, mui_tab_cursor_col,
        mui_tab_cursor_line, mui_tab_load, mui_tab_load_byte, mui_tab_open_path, mui_tab_scroll,
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
    assert_eq!(mui_tab_load(handle, 0), 4);
    assert_eq!(mui_tab_cursor_line(handle, 0), 1);
    assert_eq!(mui_tab_cursor_col(handle, 0), 0);
    assert_eq!(mui_tab_scroll(handle, 0), 0);

    // Close tab 0 -> tab 1 remains, count 1.
    mui_tab_close(handle, 0);
    assert_eq!(mui_tab_count(handle), 1);

    let _ = std::fs::remove_file(&path);
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
    use crate::{mui_tab_index_at_click, mui_tree_row_at_click};
    use crate::layout;

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

    let handle = (&mut ctx as *mut MuiContext) as usize as i64;

    // Click in the tab bar over tab 1.
    ctx.last_event = MuiEvent::mouse(crate::ffi::MUI_EVENT_MOUSE_DOWN, 0, layout::TAB_W + 5.0, 4.0, 0);
    assert_eq!(mui_tab_index_at_click(handle), 1);
    // Same x but below the tab bar -> not a tab click.
    ctx.last_event.y = layout::TAB_BAR_H + 50.0;
    assert_eq!(mui_tab_index_at_click(handle), -1);

    // Click in the sidebar over row 0.
    ctx.last_event = MuiEvent::mouse(
        crate::ffi::MUI_EVENT_MOUSE_DOWN,
        0,
        10.0,
        layout::TAB_BAR_H + 2.0,
        0,
    );
    assert_eq!(mui_tree_row_at_click(handle), 0);
    // Click right of the sidebar (in text area) -> not a tree click.
    ctx.last_event.x = layout::SIDEBAR_W + 100.0;
    assert_eq!(mui_tree_row_at_click(handle), -1);

    let _ = std::fs::remove_dir_all(&root);
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
