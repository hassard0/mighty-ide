//! glyphon-based text rendering and measurement.
//!
//! Owns a `FontSystem` + `SwashCache` + glyphon `Cache`/`Viewport`/`TextAtlas`
//! /`TextRenderer`. Per frame, [`Text::queue`] accumulates `(x, y, string,
//! color)` draw commands; [`Text::render`] shapes them into a single
//! `TextRenderer::prepare` + `render` pass.
//!
//! Fonts: the bundled **JetBrains Mono** (`fonts/*.ttf`, SIL OFL) is embedded
//! into the binary via `include_bytes!` and loaded into a fresh `FontSystem`
//! (NOT the OS default) so metrics are deterministic across machines. The
//! editor uses `theme::font_size()` (≈15px); chrome (tabs/sidebar/status) uses
//! the smaller `theme::CHROME_FONT_SIZE` via [`Text::queue_sized`].

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

use crate::ffi::MuiColor;
use crate::theme;
use crate::vello_ui::FontStyle;

/// The distinctive bundled monospace family (JetBrains Mono) — used for code.
const FONT_FAMILY: &str = "JetBrains Mono";
/// Regular + Bold + Italic + BoldItalic faces, embedded so the binary is
/// self-contained (real faces, not faux synthesis).
const FONT_REGULAR: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Bold.ttf");
const FONT_ITALIC: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Italic.ttf");
const FONT_BOLD_ITALIC: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-BoldItalic.ttf");

/// The bundled UI family (Bricolage Grotesque, SIL OFL) — used for chrome labels
/// (sidebar header, status bar, tabs, breadcrumb) to match the mockup's UI font.
const UI_FAMILY: &str = "Bricolage Grotesque";
const UI_REGULAR: &[u8] = include_bytes!("../../../fonts/BricolageGrotesque-Regular.ttf");
const UI_SEMIBOLD: &[u8] = include_bytes!("../../../fonts/BricolageGrotesque-SemiBold.ttf");
const UI_BOLD: &[u8] = include_bytes!("../../../fonts/BricolageGrotesque-Bold.ttf");

/// Default editor metrics (font size / line height in px) — LIVE from the active
/// settings (the Settings panel), so changing the editor font size re-shapes the
/// code text next frame. Functions, not consts (see `crate::settings`).
#[inline]
fn font_size() -> f32 {
    theme::FONT_SIZE()
}
#[inline]
fn line_height() -> f32 {
    theme::LINE_HEIGHT()
}

/// A queued text draw command for the current frame.
struct TextCmd {
    x: f32,
    y: f32,
    text: String,
    color: Color,
    size: f32,
    clip: Option<(i32, i32, i32, i32)>, // left, top, right, bottom
    /// `true` for overlay-layer text (palette/autocomplete/hover) drawn in a
    /// second pass on top of an opaque overlay rect, so base editor text can't
    /// bleed through. `false` for base-layer text.
    overlay: bool,
    /// `true` to shape this command in the UI family (Bricolage Grotesque)
    /// instead of the monospace code family.
    ui: bool,
    /// The face (regular / bold / italic / bold-italic) for this run.
    style: FontStyle,
}

pub struct Text {
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    renderer: TextRenderer,
    cmds: Vec<TextCmd>,
    /// When `true`, queued text is tagged as overlay-layer (see [`TextCmd`]).
    overlay: bool,
    /// When `true`, `queue_ui_sized` text shapes in the UI family.
    has_ui_font: bool,
}

fn mui_to_color(c: MuiColor) -> Color {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::rgba(to_u8(c.r), to_u8(c.g), to_u8(c.b), to_u8(c.a))
}

impl Text {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Self {
        // Build a FontSystem seeded ONLY with the bundled JetBrains Mono faces
        // (no OS fonts), so the IDE's glyphs are identical everywhere.
        let locale = "en-US".to_string();
        let mut db = glyphon::fontdb::Database::new();
        db.load_font_data(FONT_REGULAR.to_vec());
        db.load_font_data(FONT_BOLD.to_vec());
        db.load_font_data(FONT_ITALIC.to_vec());
        db.load_font_data(FONT_BOLD_ITALIC.to_vec());
        // UI family (Bricolage Grotesque) for chrome labels.
        db.load_font_data(UI_REGULAR.to_vec());
        db.load_font_data(UI_SEMIBOLD.to_vec());
        db.load_font_data(UI_BOLD.to_vec());
        db.set_monospace_family(FONT_FAMILY);
        db.set_sans_serif_family(UI_FAMILY);
        db.set_serif_family(FONT_FAMILY);
        let has_ui_font = db
            .faces()
            .any(|f| f.families.iter().any(|(name, _)| name == UI_FAMILY));
        let font_system = FontSystem::new_with_locale_and_db(locale, db);
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        Self {
            font_system,
            swash_cache,
            viewport,
            atlas,
            renderer,
            cmds: Vec::new(),
            overlay: false,
            has_ui_font,
        }
    }

    /// Drop any queued text (call at the start of a frame).
    pub fn begin(&mut self) {
        self.cmds.clear();
        self.overlay = false;
    }

    /// Push the queued text runs into a Vello [`DisplayList`] (Phase-2 render
    /// path). Each run carries its own layer (base/overlay), font family, size
    /// and color, so the Vello backend reproduces the glyphon output exactly.
    /// Consumes nothing — the runs remain queued (the Vello path is the only
    /// consumer, but keeping them lets the legacy glyphon path still work if
    /// re-enabled). Colors are converted back to floats from the packed u8.
    pub fn drain_into_display_list(&self, dl: &mut crate::vello_ui::DisplayList) {
        for cmd in &self.cmds {
            let c = cmd.color;
            let to_f = |v: u8| v as f32 / 255.0;
            let color = MuiColor::new(to_f(c.r()), to_f(c.g()), to_f(c.b()), to_f(c.a()));
            let run = crate::vello_ui::UiCmd::Text {
                x: cmd.x,
                y: cmd.y,
                text: cmd.text.clone(),
                color,
                size: cmd.size,
                ui: cmd.ui,
                style: cmd.style,
            };
            // Text clip is stored as (left, top, right, bottom) ints; convert
            // back to (x, y, w, h) floats for the Vello clip layer.
            let clip = cmd.clip.map(|(l, t, r, b)| {
                (l as f32, t as f32, (r - l) as f32, (b - t) as f32)
            });
            let entry = (run, clip);
            if cmd.overlay {
                dl.overlay.push(entry);
            } else {
                dl.base.push(entry);
            }
        }
    }

    /// Tag subsequently-queued text as overlay-layer (or base when `false`).
    pub fn set_overlay(&mut self, overlay: bool) {
        self.overlay = overlay;
    }

    /// Queue a text string to be drawn at (`x`, `y`) (baseline-top, in pixels)
    /// at the default editor font size. `clip` is an optional scissor rect
    /// (x, y, w, h) in pixels.
    pub fn queue(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: MuiColor,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        self.queue_sized(x, y, text, color, font_size(), clip);
    }

    /// Like [`Text::queue`] but at an explicit font `size` (px). Used for the
    /// smaller chrome text (tabs / sidebar / status / overlays).
    pub fn queue_sized(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: MuiColor,
        size: f32,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let clip = clip.map(|(cx, cy, cw, ch)| {
            (
                cx as i32,
                cy as i32,
                (cx + cw) as i32,
                (cy + ch) as i32,
            )
        });
        self.cmds.push(TextCmd {
            x,
            y,
            text: text.to_string(),
            color: mui_to_color(color),
            size,
            clip,
            overlay: self.overlay,
            ui: false,
            style: FontStyle::Regular,
        });
    }

    /// Like [`Text::queue_sized`] but in an explicit code-font `style` (used to
    /// render comments in italic / keywords in bold via a TRUE face).
    #[allow(clippy::too_many_arguments)]
    pub fn queue_styled(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: MuiColor,
        size: f32,
        style: FontStyle,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let clip = clip.map(|(cx, cy, cw, ch)| {
            (cx as i32, cy as i32, (cx + cw) as i32, (cy + ch) as i32)
        });
        self.cmds.push(TextCmd {
            x,
            y,
            text: text.to_string(),
            color: mui_to_color(color),
            size,
            clip,
            overlay: self.overlay,
            ui: false,
            style,
        });
    }

    /// Like [`Text::queue_sized`] but shaped in the UI family (Bricolage
    /// Grotesque) for chrome labels. Falls back to the code family if the UI
    /// font failed to load.
    pub fn queue_ui_sized(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: MuiColor,
        size: f32,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let clip = clip.map(|(cx, cy, cw, ch)| {
            (cx as i32, cy as i32, (cx + cw) as i32, (cy + ch) as i32)
        });
        self.cmds.push(TextCmd {
            x,
            y,
            text: text.to_string(),
            color: mui_to_color(color),
            size,
            clip,
            overlay: self.overlay,
            ui: self.has_ui_font,
            style: FontStyle::Regular,
        });
    }

    /// Like [`Text::queue_ui_sized`] but in an explicit UI-font `style` (bold for
    /// headers / active tab / wordmark; the UI family has no italic so italic
    /// maps to the bold face for emphasis).
    #[allow(clippy::too_many_arguments)]
    pub fn queue_ui_styled(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: MuiColor,
        size: f32,
        style: FontStyle,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let clip = clip.map(|(cx, cy, cw, ch)| {
            (cx as i32, cy as i32, (cx + cw) as i32, (cy + ch) as i32)
        });
        self.cmds.push(TextCmd {
            x,
            y,
            text: text.to_string(),
            color: mui_to_color(color),
            size,
            clip,
            overlay: self.overlay,
            ui: self.has_ui_font,
            style,
        });
    }

    /// Shape `text` and return its `(width, height)` extent in pixels.
    pub fn measure(&mut self, text: &str) -> (f32, f32) {
        let mut buffer = TextBuffer::new(
            &mut self.font_system,
            Metrics::new(font_size(), line_height()),
        );
        buffer.set_size(&mut self.font_system, None, None);
        buffer.set_text(
            &mut self.font_system,
            text,
            Attrs::new().family(Family::Name(FONT_FAMILY)),
            Shaping::Advanced,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width = 0.0f32;
        let mut lines = 0u32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        let height = (lines.max(1) as f32) * line_height();
        (width, height)
    }

    /// Build per-command cosmic-text buffers, prepare, and render the requested
    /// layer (`overlay = false` for base text, `true` for overlay-layer text).
    /// Called twice per frame so an opaque overlay rect can sit between the two
    /// layers and occlude base text.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        overlay: bool,
    ) -> Result<(), String> {
        let layer: Vec<&TextCmd> = self.cmds.iter().filter(|c| c.overlay == overlay).collect();
        if layer.is_empty() {
            return Ok(());
        }

        self.viewport.update(
            queue,
            Resolution {
                width: screen_w,
                height: screen_h,
            },
        );

        // Build one shaped buffer per command in this layer.
        let mut buffers: Vec<TextBuffer> = Vec::with_capacity(layer.len());
        for cmd in &layer {
            // Each command may have its own font size; line height tracks it at
            // the editor's ≈1.5 ratio so chrome text stays vertically centered.
            let line_h = (cmd.size * (line_height() / font_size())).max(cmd.size + 1.0);
            let mut buffer =
                TextBuffer::new(&mut self.font_system, Metrics::new(cmd.size, line_h));
            buffer.set_size(&mut self.font_system, Some(screen_w as f32), Some(screen_h as f32));
            let family = if cmd.ui { UI_FAMILY } else { FONT_FAMILY };
            buffer.set_text(
                &mut self.font_system,
                &cmd.text,
                Attrs::new().family(Family::Name(family)),
                Shaping::Advanced,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);
            buffers.push(buffer);
        }

        let areas: Vec<TextArea> = layer
            .iter()
            .zip(buffers.iter())
            .map(|(cmd, buffer)| {
                let bounds = match cmd.clip {
                    Some((l, t, r, b)) => TextBounds {
                        left: l,
                        top: t,
                        right: r,
                        bottom: b,
                    },
                    None => TextBounds {
                        left: 0,
                        top: 0,
                        right: screen_w as i32,
                        bottom: screen_h as i32,
                    },
                };
                TextArea {
                    buffer,
                    left: cmd.x,
                    top: cmd.y,
                    scale: 1.0,
                    bounds,
                    default_color: cmd.color,
                    custom_glyphs: &[],
                }
            })
            .collect();

        self.renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .map_err(|e| format!("text prepare failed: {e}"))?;

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mui text pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|e| format!("text render failed: {e}"))?;
        }

        self.atlas.trim();
        Ok(())
    }
}
