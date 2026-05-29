//! Vello GPU vector-renderer integration + a static PROOF scene.
//!
//! Phase 1 of the renderer upgrade: prove that **Vello** (a wgpu-native 2D
//! vector renderer) produces CSS-quality output that the hand-drawn solid-rect
//! pipeline in [`crate::gpu`] cannot — smooth layered radial gradients, true
//! anti-aliased rounded corners, soft (blurred) drop shadows, hairline strokes,
//! a horizontal current-line gradient, an ember caret, and AA glyph runs in the
//! bundled JetBrains Mono font.
//!
//! This is gated behind `MUI_VELLO_PROOF=1` (see [`proof_enabled`]). When set,
//! [`crate::render_and_present`] renders [`VelloProof::scene`] through Vello to
//! either the winit surface (windowed) or the offscreen texture (screenshot),
//! instead of the rect/glyphon path. The real IDE UI is untouched — that port
//! is Phase 2, gated on this proof looking great.
//!
//! Vello 0.3.0 pins `wgpu ^22.1.0`, matching the shim's wgpu 22 exactly, so the
//! Vello [`Renderer`] is constructed on the shim's existing `Device`/`Queue`
//! with no version bump.

use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill, Gradient};
use vello::skrifa::{
    instance::{LocationRef, Size as SkSize},
    metrics::GlyphMetrics,
    raw::FileRef,
    setting::VariationSetting,
    MetadataProvider,
};
use vello::{AaConfig, AaSupport, Renderer, RendererOptions, Scene};
use vello::peniko::{Blob, Font};

/// JetBrains Mono Regular, embedded so the proof is self-contained (same face the
/// glyphon path uses). Loaded into a peniko [`Font`] for Vello's glyph API.
const FONT_REGULAR: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Regular.ttf");

/// `true` when the static Vello proof scene should be rendered instead of the
/// rect/glyphon UI (env `MUI_VELLO_PROOF=1`).
pub fn proof_enabled() -> bool {
    std::env::var("MUI_VELLO_PROOF")
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0"
        })
        .unwrap_or(false)
}

/// Owns the Vello [`Renderer`] (built on the shim's wgpu device) plus the loaded
/// proof font. Constructed lazily on the first proof frame.
pub struct VelloProof {
    renderer: Renderer,
    font: Font,
}

impl VelloProof {
    /// Build the Vello renderer for `device`. `surface_format` is `Some` in
    /// windowed mode (so Vello can compile its blit pipeline for that format) and
    /// `None` for the offscreen screenshot path.
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

        let font = Font::new(Blob::new(std::sync::Arc::new(FONT_REGULAR)), 0);
        Ok(Self { renderer, font })
    }

    /// Render the proof [`Scene`] to an offscreen [`wgpu::TextureView`] (the
    /// texture must be `Rgba8Unorm` + `STORAGE_BINDING`, which the offscreen
    /// target is). Used by the screenshot path.
    pub fn render_to_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let scene = self.build_scene(width, height);
        self.renderer
            .render_to_texture(
                device,
                queue,
                &scene,
                view,
                &vello::RenderParams {
                    base_color: aurora_base(),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| format!("Vello render_to_texture failed: {e}"))
    }

    /// Render the proof scene to a winit surface texture (Vello renders to an
    /// internal texture then blits to the surface, handling sRGB conversion).
    pub fn render_to_surface(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: &wgpu::SurfaceTexture,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let scene = self.build_scene(width, height);
        self.renderer
            .render_to_surface(
                device,
                queue,
                &scene,
                surface,
                &vello::RenderParams {
                    base_color: aurora_base(),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| format!("Vello render_to_surface failed: {e}"))
    }

    /// Build the static Aurora Noir proof scene at `(w, h)`.
    fn build_scene(&self, w: u32, h: u32) -> Scene {
        let mut scene = Scene::new();
        let wf = w as f64;
        let hf = h as f64;

        // --- 1. Atmospheric background: layered radial gradients over the base.
        // base_color in RenderParams already paints #0c0e13; we lay the three
        // aurora glows on top, each fading to transparent so they blend smoothly.
        paint_radial(
            &mut scene,
            Point::new(wf * 0.12, hf * -0.08),
            wf * 0.78,
            Color::rgba8(0x1a, 0x27, 0x40, 0xff),
            wf,
            hf,
        );
        paint_radial(
            &mut scene,
            Point::new(wf * 1.0, hf * 0.0),
            wf * 0.62,
            Color::rgba8(0x2a, 0x1c, 0x2e, 0xff),
            wf,
            hf,
        );
        paint_radial(
            &mut scene,
            Point::new(wf * 0.6, hf * 1.2),
            wf * 0.85,
            Color::rgba8(0x10, 0x20, 0x2a, 0xff),
            wf,
            hf,
        );

        // --- 2. Rounded panel (#13161e, ~14px radius) with a soft drop shadow
        // and a 1px hairline border (#2a3140).
        let panel = Rect::new(120.0, 150.0, wf - 120.0, hf - 130.0);
        let radius = 14.0;

        // Soft drop shadow: a blurred rounded rect offset down/under the panel.
        scene.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            Rect::new(panel.x0 - 4.0, panel.y0 + 22.0, panel.x1 + 4.0, panel.y1 + 30.0),
            Color::rgba8(0, 0, 0, 0xcc),
            radius,
            30.0, // std_dev — generous, soft penumbra
        );

        let panel_round = RoundedRect::from_rect(panel, radius);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::rgba8(0x13, 0x16, 0x1e, 0xff),
            None,
            &panel_round,
        );
        // 1px hairline border.
        scene.stroke(
            &Stroke::new(1.0),
            Affine::IDENTITY,
            Color::rgba8(0x2a, 0x31, 0x40, 0xff),
            None,
            &panel_round,
        );

        // --- 3. Current-line band: a soft horizontal gradient (ember -> clear),
        // spanning a code row inside the panel.
        let line_top = panel.y0 + 150.0;
        let line_h = 24.0;
        let band = Rect::new(panel.x0 + 1.0, line_top, panel.x1 - 1.0, line_top + line_h);
        let band_grad = Gradient::new_linear(
            Point::new(band.x0, 0.0),
            Point::new(band.x0 + (band.x1 - band.x0) * 0.5, 0.0),
        )
        .with_stops([
            (0.0, Color::rgba8(0xf4, 0xa2, 0x59, 0x26)),
            (1.0, Color::rgba8(0xf4, 0xa2, 0x59, 0x00)),
        ]);
        scene.fill(Fill::NonZero, Affine::IDENTITY, &band_grad, None, &band);

        // --- 4. Ember accent bar (left edge of the current line).
        let accent = RoundedRect::new(
            panel.x0 + 1.0,
            line_top + 2.0,
            panel.x0 + 4.0,
            line_top + line_h - 2.0,
            1.5,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::rgba8(0xf4, 0xa2, 0x59, 0xff),
            None,
            &accent,
        );

        // --- 5. Anti-aliased text: a syntax-colored code line + a UI label.
        let code_x = (panel.x0 + 36.0) as f32;
        let font_px = 22.0_f32;
        // Syntax-colored tokens, laid out left-to-right at the monospace advance.
        // (violet keyword, teal type, sage string, ember number).
        let tokens: &[(&str, Color)] = &[
            ("let ", Color::rgba8(0xec, 0xe6, 0xda, 0xff)), // text
            ("label", Color::rgba8(0xec, 0xe6, 0xda, 0xff)),
            (" = ", Color::rgba8(0x9a, 0xa3, 0xb2, 0xff)), // punctuation
            ("classify", Color::rgba8(0x7f, 0xb0, 0xe8, 0xff)), // fn (sky)
            ("(", Color::rgba8(0x9a, 0xa3, 0xb2, 0xff)),
            ("42", Color::rgba8(0xf4, 0xa2, 0x59, 0xff)), // ember number
            (")", Color::rgba8(0x9a, 0xa3, 0xb2, 0xff)),
        ];
        let mut pen_x = code_x;
        for (s, color) in tokens {
            pen_x = self.draw_text(
                &mut scene,
                s,
                pen_x,
                (line_top + line_h * 0.5) as f32 + font_px * 0.35,
                font_px,
                *color,
            );
        }
        // End-of-current-line pen position — where the ember caret sits.
        let caret_x = pen_x;

        // A second code line above, demonstrating keyword (violet) + type (teal)
        // + string (sage).
        let kw_y = (line_top - line_h - 6.0) as f32 + font_px * 0.35;
        let kw_tokens: &[(&str, Color)] = &[
            ("fn ", Color::rgba8(0xb9, 0x9c, 0xf5, 0xff)), // keyword violet
            ("classify", Color::rgba8(0x7f, 0xb0, 0xe8, 0xff)),
            ("(n: ", Color::rgba8(0x9a, 0xa3, 0xb2, 0xff)),
            ("I32", Color::rgba8(0x56, 0xc7, 0xc0, 0xff)), // type teal/aurora
            (") -> ", Color::rgba8(0x9a, 0xa3, 0xb2, 0xff)),
            ("Str", Color::rgba8(0x56, 0xc7, 0xc0, 0xff)),
        ];
        let mut pen_x = code_x;
        for (s, color) in kw_tokens {
            pen_x = self.draw_text(&mut scene, s, pen_x, kw_y, font_px, *color);
        }

        // A string-literal line below, in sage.
        let str_y = (line_top + line_h * 2.0 + 6.0) as f32 + font_px * 0.35;
        let mut pen_x = code_x;
        for (s, color) in &[
            ("  return ", Color::rgba8(0xb9, 0x9c, 0xf5, 0xff)),
            ("\"positive\"", Color::rgba8(0xa9, 0xc7, 0x7e, 0xff)), // string sage
        ] {
            pen_x = self.draw_text(&mut scene, s, pen_x, str_y, font_px, *color);
        }
        let _ = pen_x;

        // UI label (panel title), in the warm off-white, smaller.
        self.draw_text(
            &mut scene,
            "demo.mty  —  Vello proof",
            panel.x0 as f32 + 36.0,
            panel.y0 as f32 + 44.0,
            15.0,
            Color::rgba8(0xec, 0xe6, 0xda, 0xff),
        );

        // --- 6. Ember caret at the end of the current line (a thin rounded bar
        // glowing in the ember accent, sitting just past the line's last glyph).
        let caret = RoundedRect::new(
            caret_x as f64 + 2.0,
            line_top + 3.0,
            caret_x as f64 + 4.0,
            line_top + line_h - 3.0,
            1.0,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::rgba8(0xf4, 0xa2, 0x59, 0xff),
            None,
            &caret,
        );

        scene
    }

    /// Draw a monospace text run starting at baseline `(x, y_baseline)` and
    /// return the pen x after the run. Uses skrifa to map chars -> glyph ids and
    /// advance by the font's per-glyph advance width (JetBrains Mono is
    /// monospace, so advances are uniform but we measure honestly).
    fn draw_text(
        &self,
        scene: &mut Scene,
        text: &str,
        x: f32,
        y_baseline: f32,
        size_px: f32,
        color: Color,
    ) -> f32 {
        let font_ref = {
            let file = FileRef::new(self.font.data.as_ref()).expect("valid font file");
            match file {
                FileRef::Font(f) => f,
                FileRef::Collection(c) => c.get(self.font.index).expect("font in collection"),
            }
        };
        let axes: Vec<VariationSetting> = Vec::new();
        let _ = &axes; // no variable-font axes for this static face
        let charmap = font_ref.charmap();
        let metrics = GlyphMetrics::new(&font_ref, SkSize::new(size_px), LocationRef::default());

        let mut pen_x = x;
        let glyphs: Vec<vello::Glyph> = text
            .chars()
            .map(|c| {
                let gid = charmap.map(c).unwrap_or_default();
                let advance = metrics.advance_width(gid).unwrap_or(size_px * 0.6);
                let g = vello::Glyph {
                    id: gid.to_u32(),
                    x: pen_x,
                    y: y_baseline,
                };
                pen_x += advance;
                g
            })
            .collect();

        scene
            .draw_glyphs(&self.font)
            .font_size(size_px)
            .brush(color)
            .hint(false)
            .draw(Fill::NonZero, glyphs.into_iter());

        pen_x
    }
}

/// The Aurora Noir window base color (#0c0e13) — Vello's `base_color`.
fn aurora_base() -> Color {
    Color::rgba8(0x0c, 0x0e, 0x13, 0xff)
}

/// Fill the whole canvas with a radial glow that fades from `color` at `center`
/// to fully transparent at `radius`, blending over whatever is beneath it.
fn paint_radial(scene: &mut Scene, center: Point, radius: f64, color: Color, w: f64, h: f64) {
    let grad = Gradient::new_radial(center, radius as f32).with_stops([
        (0.0, color),
        (
            0.55,
            Color::rgba8(color.r, color.g, color.b, (color.a as f32 * 0.45) as u8),
        ),
        (1.0, Color::rgba8(color.r, color.g, color.b, 0x00)),
    ]);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &grad,
        None,
        &Rect::new(0.0, 0.0, w, h),
    );
}
