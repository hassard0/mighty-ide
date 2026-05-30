//! Vello GPU vector-renderer backend for the ENTIRE Mighty IDE UI (Phase 2).
//!
//! Phase 1 ([`crate::vello_proof`]) proved Vello produces CSS-quality output on
//! the shim's wgpu-22 device. Phase 2 (this module) renders the WHOLE IDE
//! chrome and editor through Vello, retiring the solid-rect pipeline + glyphon
//! as the default render path.
//!
//! ## Model
//!
//! Each frame the chrome/editor draw functions (in `abi.rs`, `palette.rs`,
//! `completion.rs`, `nav.rs`) push rich 2D primitives into a per-frame
//! [`DisplayList`] held on [`crate::MuiContext`] — rounded rects, horizontal
//! gradients, soft (blurred) drop shadows, hairline strokes, wavy diagnostic
//! squiggles, and anti-aliased glyph runs in JetBrains Mono (code) or Bricolage
//! Grotesque (UI chrome). The list has a base layer and an overlay layer (drawn
//! on top, so palette/autocomplete cards occlude editor glyphs), mirroring the
//! old rect/glyphon two-pass scheme.
//!
//! [`VelloUi::render`] then replays the display list into a single
//! [`vello::Scene`], laid over the layered "Aurora Noir" radial-gradient
//! atmosphere, and renders that scene to the winit surface (windowed) or the
//! offscreen screenshot texture. Vello owns its own GPU submission.
//!
//! All backend STATE (text model, tabs, tree, diagnostics, LSP, terminal, undo)
//! is unchanged — only the rendering primitives differ.

use std::sync::Arc;

use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Blob, Color, Fill, Font, Gradient};
use vello::skrifa::{
    instance::{LocationRef, Size as SkSize},
    metrics::GlyphMetrics,
    raw::FileRef,
    MetadataProvider,
};
use vello::{AaConfig, AaSupport, Renderer, RendererOptions, Scene};

use crate::ffi::MuiColor;

// Bundled fonts (same faces glyphon used), embedded so the binary is
// self-contained. JetBrains Mono = code + monospace chrome; Bricolage Grotesque
// = UI labels (headers, wordmark, status, badges). Each family carries its REAL
// bold / italic faces (no faux synthesis) so emphasis renders as a true face.
const FONT_CODE: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Regular.ttf");
const FONT_CODE_BOLD: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Bold.ttf");
const FONT_CODE_ITALIC: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Italic.ttf");
const FONT_CODE_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../fonts/JetBrainsMono-BoldItalic.ttf");
const FONT_UI: &[u8] = include_bytes!("../../../fonts/BricolageGrotesque-SemiBold.ttf");
const FONT_UI_BOLD: &[u8] = include_bytes!("../../../fonts/BricolageGrotesque-Bold.ttf");

/// The face a glyph run is drawn in. `Bold` / `Italic` select a TRUE bundled
/// face (not a faux/synthesized slant or weight). `BoldItalic` is the combined
/// face; the UI family (Bricolage) has no italic so italic UI text falls back to
/// the bold UI face for emphasis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    /// Combine a base style with bold / italic flags (used when flattening
    /// markdown spans that may nest `**_x_**`).
    pub fn with(self, bold: bool, italic: bool) -> FontStyle {
        let b = bold || matches!(self, FontStyle::Bold | FontStyle::BoldItalic);
        let i = italic || matches!(self, FontStyle::Italic | FontStyle::BoldItalic);
        match (b, i) {
            (true, true) => FontStyle::BoldItalic,
            (true, false) => FontStyle::Bold,
            (false, true) => FontStyle::Italic,
            (false, false) => FontStyle::Regular,
        }
    }
}

/// Convert a shim [`MuiColor`] (0..=1 floats) to a Vello/peniko [`Color`].
#[inline]
fn col(c: MuiColor) -> Color {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::rgba8(to_u8(c.r), to_u8(c.g), to_u8(c.b), to_u8(c.a))
}

/// One drawing primitive in the per-frame display list. Pixel space, top-left
/// origin (matching the old rect/text ABI exactly so all layout math is reused).
#[derive(Clone)]
pub enum UiCmd {
    /// A flat filled rectangle (the `mui_fill_rect` primitive).
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: MuiColor,
    },
    /// A filled rounded rectangle.
    RoundRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
    },
    /// A rounded rectangle filled with a left→right horizontal gradient that
    /// fades from `color` to transparent across `fade` (0..1) of its width
    /// (used for the current-line band + selected-row tints).
    GradH {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
        fade: f32,
    },
    /// A rounded rectangle filled with a top→bottom vertical gradient between
    /// two colors (elevated panels/cards).
    GradV {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        top: MuiColor,
        bottom: MuiColor,
    },
    /// A soft (blurred) drop shadow under a rounded rect.
    Shadow {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
        blur: f32,
    },
    /// A 1px hairline stroke around a rounded rect (borders).
    StrokeRound {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
        width: f32,
    },
    /// A radial glow filled over its bounding rect, fading center→edge. Used for
    /// the ember brand tile + soft accent glows.
    #[allow(dead_code)]
    RadialGlow {
        cx: f32,
        cy: f32,
        radius: f32,
        inner: MuiColor,
        outer: MuiColor,
        clip_x: f32,
        clip_y: f32,
        clip_w: f32,
        clip_h: f32,
    },
    /// A wavy (sine) underline stroke — the red diagnostic squiggle.
    Squiggle {
        x: f32,
        y: f32,
        w: f32,
        color: MuiColor,
    },
    /// A real vector icon: an SVG path (24x24 viewBox by convention) scaled into
    /// the box `(x, y, w, h)`, then stroked (and/or filled) in `color`. This is
    /// the crisp-icon primitive that replaces unicode-glyph "icons".
    Icon {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        /// SVG path `d=` data, in a 0..`vb` user-space box.
        path: &'static str,
        color: MuiColor,
        /// Stroke width in *target* pixels (0 = fill only).
        stroke: f32,
        /// When `true`, also fill the path (used for solid badges/play tris).
        fill: bool,
        /// The source viewBox edge length (typically 24.0).
        vb: f32,
    },
    /// An anti-aliased monospace/UI text run at baseline-top `(x, y)`.
    Text {
        x: f32,
        y: f32,
        text: String,
        color: MuiColor,
        size: f32,
        /// `true` shapes in the UI family (Bricolage Grotesque) rather than code.
        ui: bool,
        /// The face (regular / bold / italic / bold-italic) to shape this run in.
        style: FontStyle,
    },
}

/// An optional scissor clip (x, y, w, h in pixels) carried alongside a command.
pub type Clip = Option<(f32, f32, f32, f32)>;

/// A two-layer per-frame display list. The base layer holds the editor + chrome;
/// the overlay layer holds palette / autocomplete / hover (drawn on top so its
/// cards occlude base glyphs, replacing the old overlay rect/text passes). Each
/// command carries an optional clip rect (used by the editor prompt/find region
/// + honored by the Vello backend via a clip layer).
#[derive(Default)]
pub struct DisplayList {
    pub base: Vec<(UiCmd, Clip)>,
    pub overlay: Vec<(UiCmd, Clip)>,
    /// When `true`, `push` routes into `overlay`.
    pub on_overlay: bool,
    /// The active clip rect applied to subsequently-pushed commands.
    pub clip: Clip,
}

impl DisplayList {
    pub fn clear(&mut self) {
        self.base.clear();
        self.overlay.clear();
        self.on_overlay = false;
        self.clip = None;
    }
    #[inline]
    pub fn push(&mut self, cmd: UiCmd) {
        let entry = (cmd, self.clip);
        if self.on_overlay {
            self.overlay.push(entry);
        } else {
            self.base.push(entry);
        }
    }
}

/// Owns the Vello [`Renderer`] (built on the shim's wgpu device) + the two loaded
/// fonts. Constructed lazily on the first frame.
pub struct VelloUi {
    renderer: Renderer,
    code: Font,
    code_bold: Font,
    code_italic: Font,
    code_bold_italic: Font,
    ui: Font,
    ui_bold: Font,
}

impl VelloUi {
    pub fn new(
        device: &wgpu::Device,
        surface_format: Option<wgpu::TextureFormat>,
    ) -> Result<Self, String> {
        let renderer = Renderer::new(
            device,
            RendererOptions {
                surface_format,
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: std::num::NonZeroUsize::new(1),
            },
        )
        .map_err(|e| format!("Vello Renderer::new failed: {e}"))?;
        let code = Font::new(Blob::new(Arc::new(FONT_CODE)), 0);
        let code_bold = Font::new(Blob::new(Arc::new(FONT_CODE_BOLD)), 0);
        let code_italic = Font::new(Blob::new(Arc::new(FONT_CODE_ITALIC)), 0);
        let code_bold_italic = Font::new(Blob::new(Arc::new(FONT_CODE_BOLD_ITALIC)), 0);
        let ui = Font::new(Blob::new(Arc::new(FONT_UI)), 0);
        let ui_bold = Font::new(Blob::new(Arc::new(FONT_UI_BOLD)), 0);
        Ok(Self {
            renderer,
            code,
            code_bold,
            code_italic,
            code_bold_italic,
            ui,
            ui_bold,
        })
    }

    /// Resolve a `(ui, style)` pair to the real bundled face. Code has all four
    /// faces; the UI family has only regular + bold, so any italic UI request
    /// uses the bold UI face (emphasis) — never a faux slant.
    fn face(&self, ui: bool, style: FontStyle) -> &Font {
        if ui {
            match style {
                FontStyle::Regular | FontStyle::Italic => &self.ui,
                FontStyle::Bold | FontStyle::BoldItalic => &self.ui_bold,
            }
        } else {
            match style {
                FontStyle::Regular => &self.code,
                FontStyle::Bold => &self.code_bold,
                FontStyle::Italic => &self.code_italic,
                FontStyle::BoldItalic => &self.code_bold_italic,
            }
        }
    }

    /// The window base / GPU clear color — the active theme's `bg`.
    fn base_color() -> Color {
        col(crate::theme::BG())
    }

    /// Render `dl` (laid over the atmosphere) to an offscreen texture. `width` /
    /// `height` are the PHYSICAL target size; the display list is in LOGICAL px,
    /// so the whole scene is scaled by `ui_scale` to fill the physical target.
    pub fn render_to_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        dl: &DisplayList,
    ) -> Result<(), String> {
        let scene = self.build_scene(width, height, dl);
        self.renderer
            .render_to_texture(
                device,
                queue,
                &scene,
                view,
                &vello::RenderParams {
                    base_color: Self::base_color(),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| format!("Vello render_to_texture failed: {e}"))
    }

    /// Render `dl` to the winit surface texture. `width` / `height` are the
    /// PHYSICAL surface size; the display list is in LOGICAL px and is scaled by
    /// `ui_scale` to fill it (so the whole UI honors the OS scale + user zoom).
    pub fn render_to_surface(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: &wgpu::SurfaceTexture,
        width: u32,
        height: u32,
        dl: &DisplayList,
    ) -> Result<(), String> {
        let scene = self.build_scene(width, height, dl);
        self.renderer
            .render_to_surface(
                device,
                queue,
                &scene,
                surface,
                &vello::RenderParams {
                    base_color: Self::base_color(),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| format!("Vello render_to_surface failed: {e}"))
    }

    /// Build the full Vello scene at the PHYSICAL target size (`pw`×`ph`). The
    /// display list + atmosphere are authored in LOGICAL px, so everything is
    /// painted into a logical-sized child scene which is then appended under an
    /// `ui_scale` transform — scaling the entire UI (chrome + editor + glyphs)
    /// uniformly to the physical surface. `ui_scale == 1.0` reproduces the
    /// historical (offscreen/screenshot) output exactly.
    fn build_scene(&self, pw: u32, ph: u32, dl: &DisplayList) -> Scene {
        let scale = crate::uiscale::ui_scale() as f64;
        let lw = (pw as f64 / scale).max(1.0);
        let lh = (ph as f64 / scale).max(1.0);

        let mut inner = Scene::new();
        self.paint_atmosphere(&mut inner, lw, lh);
        for (cmd, clip) in &dl.base {
            self.paint_clipped(&mut inner, cmd, *clip);
        }
        for (cmd, clip) in &dl.overlay {
            self.paint_clipped(&mut inner, cmd, *clip);
        }

        if (scale - 1.0).abs() < f64::EPSILON {
            return inner;
        }
        let mut scene = Scene::new();
        scene.append(&inner, Some(Affine::scale(scale)));
        scene
    }

    /// Paint a command, wrapping it in a rectangular clip layer when `clip` is
    /// set (used by the editor prompt/find region).
    fn paint_clipped(&self, scene: &mut Scene, cmd: &UiCmd, clip: Clip) {
        match clip {
            Some((cx, cy, cw, ch)) if cw > 0.0 && ch > 0.0 => {
                let r = Rect::new(
                    cx as f64,
                    cy as f64,
                    (cx + cw) as f64,
                    (cy + ch) as f64,
                );
                scene.push_layer(vello::peniko::Mix::Clip, 1.0, Affine::IDENTITY, &r);
                self.paint_cmd(scene, cmd);
                scene.pop_layer();
            }
            // A zero-area clip clips everything: draw nothing.
            Some(_) => {}
            None => self.paint_cmd(scene, cmd),
        }
    }

    /// The layered atmosphere painted behind the whole window, read from the
    /// active theme's `atmosphere` radial stops (each a glow over the base color
    /// already painted by `base_color`). Themes vary this: Vivid = cool-blue /
    /// magenta / teal noir; Aurora = teal/indigo/violet aurora; Warm = soft warm
    /// paper washes. On a light theme the glows are near-opaque blends rather
    /// than additive screens, so they read as warm paper rather than dark glow.
    fn paint_atmosphere(&self, scene: &mut Scene, w: f64, h: f64) {
        let theme = crate::theme::active();
        let light = theme.is_light;
        for stop in theme.atmosphere.iter() {
            if stop.radius <= 0.0 || stop.color.a <= 0.0 {
                continue;
            }
            radial_themed(
                scene,
                Point::new(w * stop.cx as f64, h * stop.cy as f64),
                w * stop.radius as f64,
                col(stop.color),
                w,
                h,
                light,
            );
        }
    }

    fn paint_cmd(&self, scene: &mut Scene, cmd: &UiCmd) {
        match cmd {
            UiCmd::Rect { x, y, w, h, color } => {
                if *w <= 0.0 || *h <= 0.0 {
                    return;
                }
                let r = Rect::new(*x as f64, *y as f64, (*x + *w) as f64, (*y + *h) as f64);
                scene.fill(Fill::NonZero, Affine::IDENTITY, col(*color), None, &r);
            }
            UiCmd::RoundRect {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => {
                if *w <= 0.0 || *h <= 0.0 {
                    return;
                }
                let rr = RoundedRect::new(
                    *x as f64,
                    *y as f64,
                    (*x + *w) as f64,
                    (*y + *h) as f64,
                    (*radius as f64).min((w.min(*h) as f64) * 0.5),
                );
                scene.fill(Fill::NonZero, Affine::IDENTITY, col(*color), None, &rr);
            }
            UiCmd::GradH {
                x,
                y,
                w,
                h,
                radius,
                color,
                fade,
            } => {
                if *w <= 0.0 || *h <= 0.0 {
                    return;
                }
                let c = col(*color);
                let clear = Color::rgba8(c.r, c.g, c.b, 0);
                let span = (*w as f64) * (*fade as f64).clamp(0.05, 1.0);
                let grad = Gradient::new_linear(
                    Point::new(*x as f64, 0.0),
                    Point::new(*x as f64 + span, 0.0),
                )
                .with_stops([(0.0, c), (1.0, clear)]);
                let rr = RoundedRect::new(
                    *x as f64,
                    *y as f64,
                    (*x + *w) as f64,
                    (*y + *h) as f64,
                    *radius as f64,
                );
                scene.fill(Fill::NonZero, Affine::IDENTITY, &grad, None, &rr);
            }
            UiCmd::GradV {
                x,
                y,
                w,
                h,
                radius,
                top,
                bottom,
            } => {
                if *w <= 0.0 || *h <= 0.0 {
                    return;
                }
                let grad = Gradient::new_linear(
                    Point::new(0.0, *y as f64),
                    Point::new(0.0, (*y + *h) as f64),
                )
                .with_stops([(0.0, col(*top)), (1.0, col(*bottom))]);
                let rr = RoundedRect::new(
                    *x as f64,
                    *y as f64,
                    (*x + *w) as f64,
                    (*y + *h) as f64,
                    *radius as f64,
                );
                scene.fill(Fill::NonZero, Affine::IDENTITY, &grad, None, &rr);
            }
            UiCmd::Shadow {
                x,
                y,
                w,
                h,
                radius,
                color,
                blur,
            } => {
                scene.draw_blurred_rounded_rect(
                    Affine::IDENTITY,
                    Rect::new(*x as f64, *y as f64, (*x + *w) as f64, (*y + *h) as f64),
                    col(*color),
                    *radius as f64,
                    *blur as f64,
                );
            }
            UiCmd::StrokeRound {
                x,
                y,
                w,
                h,
                radius,
                color,
                width,
            } => {
                let rr = RoundedRect::new(
                    *x as f64,
                    *y as f64,
                    (*x + *w) as f64,
                    (*y + *h) as f64,
                    *radius as f64,
                );
                scene.stroke(
                    &Stroke::new(*width as f64),
                    Affine::IDENTITY,
                    col(*color),
                    None,
                    &rr,
                );
            }
            UiCmd::RadialGlow {
                cx,
                cy,
                radius,
                inner,
                outer,
                clip_x,
                clip_y,
                clip_w,
                clip_h,
            } => {
                let grad = Gradient::new_radial(Point::new(*cx as f64, *cy as f64), *radius)
                    .with_stops([(0.0, col(*inner)), (1.0, col(*outer))]);
                let r = Rect::new(
                    *clip_x as f64,
                    *clip_y as f64,
                    (*clip_x + *clip_w) as f64,
                    (*clip_y + *clip_h) as f64,
                );
                scene.fill(Fill::NonZero, Affine::IDENTITY, &grad, None, &r);
            }
            UiCmd::Squiggle { x, y, w, color } => {
                let mut path = BezPath::new();
                let amp = 1.6_f64;
                let period = 6.0_f64;
                let x0 = *x as f64;
                let y0 = *y as f64;
                let wf = (*w as f64).max(period);
                path.move_to(Point::new(x0, y0));
                let mut px = 0.0;
                let mut up = true;
                while px < wf {
                    let nx = (px + period * 0.5).min(wf);
                    let cx = x0 + (px + nx) * 0.5;
                    let cy = if up { y0 - amp } else { y0 + amp };
                    path.quad_to(Point::new(cx, cy), Point::new(x0 + nx, y0));
                    px = nx;
                    up = !up;
                }
                scene.stroke(
                    &Stroke::new(1.4),
                    Affine::IDENTITY,
                    col(*color),
                    None,
                    &path,
                );
            }
            UiCmd::Icon {
                x,
                y,
                w,
                h,
                path,
                color,
                stroke,
                fill,
                vb,
            } => {
                let Ok(bez) = BezPath::from_svg(path) else {
                    return;
                };
                // Scale the viewBox box into the target box, preserving aspect by
                // using a uniform scale (the smaller of the two) and centering.
                let vb = (*vb as f64).max(1.0);
                let sx = (*w as f64) / vb;
                let sy = (*h as f64) / vb;
                let s = sx.min(sy);
                let off_x = *x as f64 + ((*w as f64) - vb * s) * 0.5;
                let off_y = *y as f64 + ((*h as f64) - vb * s) * 0.5;
                let aff = Affine::translate((off_x, off_y)) * Affine::scale(s);
                let c = col(*color);
                if *fill {
                    scene.fill(Fill::NonZero, aff, c, None, &bez);
                }
                if *stroke > 0.0 {
                    // Stroke width is specified in target px; divide by the scale
                    // so the affine doesn't double-scale it.
                    let sw = (*stroke as f64) / s.max(1e-6);
                    let st = Stroke::new(sw)
                        .with_caps(vello::kurbo::Cap::Round)
                        .with_join(vello::kurbo::Join::Round);
                    scene.stroke(&st, aff, c, None, &bez);
                }
            }
            UiCmd::Text {
                x,
                y,
                text,
                color,
                size,
                ui,
                style,
            } => {
                self.draw_text(scene, text, *x, *y, *size, col(*color), *ui, *style);
            }
        }
    }

    /// Draw a text run. `y` is the baseline-top (as the old text ABI used), so we
    /// shift down by the font ascent to put glyphs on a proper baseline. Code
    /// uses the monospace cell advance (matches `layout::CHAR_W()` proportionally);
    /// UI text uses real per-glyph advances for proportional shaping.
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &self,
        scene: &mut Scene,
        text: &str,
        x: f32,
        y_top: f32,
        size_px: f32,
        color: Color,
        ui: bool,
        style: FontStyle,
    ) {
        if text.is_empty() {
            return;
        }
        let font = self.face(ui, style);
        let font_ref = {
            let file = match FileRef::new(font.data.as_ref()) {
                Ok(f) => f,
                Err(_) => return,
            };
            match file {
                FileRef::Font(f) => f,
                FileRef::Collection(c) => match c.get(font.index) {
                    Ok(f) => f,
                    Err(_) => return,
                },
            }
        };
        let charmap = font_ref.charmap();
        let metrics = GlyphMetrics::new(&font_ref, SkSize::new(size_px), LocationRef::default());
        // Baseline: shift the top-anchored y down by ~0.80em (JetBrains Mono /
        // Bricolage cap+ascent), tuned so glyphs sit centered in the old line box.
        let baseline = y_top + size_px * 0.80;
        // For code, force a uniform monospace advance equal to the editor cell
        // so glyph x positions line up exactly with carets/selections/gutter.
        let mono_advance = crate::theme::CHAR_W() * (size_px / crate::theme::FONT_SIZE());

        let mut pen_x = x;
        let glyphs: Vec<vello::Glyph> = text
            .chars()
            .map(|c| {
                let gid = charmap.map(c).unwrap_or_default();
                let g = vello::Glyph {
                    id: gid.to_u32(),
                    x: pen_x,
                    y: baseline,
                };
                let adv = if ui {
                    metrics.advance_width(gid).unwrap_or(size_px * 0.5)
                } else {
                    mono_advance
                };
                pen_x += adv;
                g
            })
            .collect();

        scene
            .draw_glyphs(font)
            .font_size(size_px)
            .brush(color)
            .hint(false)
            .draw(Fill::NonZero, glyphs.into_iter());
    }
}

/// Radial atmosphere stop. On a DARK theme the stop fades color→transparent so
/// it reads as an additive glow over the near-black base. On a LIGHT (paper)
/// theme the wash color is near the paper hue and is given a low overall alpha
/// so it tints the paper gently rather than darkening it — soft warm light.
fn radial_themed(
    scene: &mut Scene,
    center: Point,
    radius: f64,
    color: Color,
    w: f64,
    h: f64,
    light: bool,
) {
    let stops: [(f32, Color); 3] = if light {
        // Gentle paper tint: the wash at low alpha at the center, fading out.
        [
            (0.0, Color::rgba8(color.r, color.g, color.b, 0x9e)),
            (0.6, Color::rgba8(color.r, color.g, color.b, 0x3c)),
            (1.0, Color::rgba8(color.r, color.g, color.b, 0x00)),
        ]
    } else {
        [
            (0.0, color),
            (
                0.5,
                Color::rgba8(color.r, color.g, color.b, (color.a as f32 * 0.42) as u8),
            ),
            (1.0, Color::rgba8(color.r, color.g, color.b, 0x00)),
        ]
    };
    let grad = Gradient::new_radial(center, radius as f32).with_stops(stops);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &grad,
        None,
        &Rect::new(0.0, 0.0, w, h),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons;

    /// Every icon in the registry must be valid SVG path data that `kurbo` can
    /// parse into a non-empty `BezPath` — the contract the `Icon` primitive
    /// relies on (a parse failure silently draws nothing).
    #[test]
    fn all_registry_icons_parse_to_nonempty_paths() {
        let all: &[(&str, &str)] = &[
            ("EXPLORER", icons::EXPLORER),
            ("SEARCH", icons::SEARCH),
            ("GIT", icons::GIT),
            ("RUN", icons::RUN),
            ("AGENTS", icons::AGENTS),
            ("AGENTS_DOT", icons::AGENTS_DOT),
            ("USER", icons::USER),
            ("SETTINGS", icons::SETTINGS),
            ("FOLDER", icons::FOLDER),
            ("FILE_MTY", icons::FILE_MTY),
            ("FILE_TOML", icons::FILE_TOML),
            ("FILE_MD", icons::FILE_MD),
            ("FILE_TXT", icons::FILE_TXT),
            ("CHEVRON", icons::CHEVRON),
            ("CLOSE", icons::CLOSE),
            ("DOTS", icons::DOTS),
            ("NEW_FILE", icons::NEW_FILE),
            ("NEW_FOLDER", icons::NEW_FOLDER),
            ("COLLAPSE", icons::COLLAPSE),
            ("FN_SYMBOL", icons::FN_SYMBOL),
            ("BRANCH", icons::BRANCH),
            ("PLUS", icons::PLUS),
            ("ERROR_CIRCLE", icons::ERROR_CIRCLE),
            ("WARN_TRI", icons::WARN_TRI),
            ("LF", icons::LF),
            ("LANG_M", icons::LANG_M),
            ("BELL", icons::BELL),
            ("TEST_BOX", icons::TEST_BOX),
            ("INFO_I", icons::INFO_I),
        ];
        for (name, d) in all {
            let bez = BezPath::from_svg(d)
                .unwrap_or_else(|e| panic!("icon `{name}` failed to parse: {e}"));
            assert!(
                !bez.elements().is_empty(),
                "icon `{name}` parsed to an empty path"
            );
        }
    }

    /// An `Icon` scaled into a target box paints into the scene without
    /// panicking (smoke test over the affine + stroke math).
    #[test]
    fn icon_paints_into_scene() {
        let mut scene = Scene::new();
        let bez = BezPath::from_svg(icons::FILE_MTY).expect("parse");
        let aff = Affine::translate((10.0, 10.0)) * Affine::scale(14.0 / 24.0);
        scene.stroke(
            &Stroke::new(2.0),
            aff,
            Color::rgba8(0x9d, 0x83, 0xff, 0xff),
            None,
            &bez,
        );
    }
}


