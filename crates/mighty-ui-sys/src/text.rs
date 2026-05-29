//! glyphon-based text rendering and measurement.
//!
//! Owns a `FontSystem` + `SwashCache` + glyphon `Cache`/`Viewport`/`TextAtlas`
//! /`TextRenderer`. Per frame, [`Text::queue`] accumulates `(x, y, string,
//! color)` draw commands; [`Text::render`] shapes them into a single
//! `TextRenderer::prepare` + `render` pass.
//!
//! Fonts: loaded from the system font database via cosmic-text's default
//! `FontSystem::new()` (which uses fontdb's system source). A monospace family
//! is requested by generic `Family::Monospace`. Bundling a `.ttf` for fully
//! deterministic metrics across machines is a later nicety (see plan Task 2.3).

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

use crate::ffi::MuiColor;

/// Default monospace metrics (font size / line height in px).
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 20.0;

/// A queued text draw command for the current frame.
struct TextCmd {
    x: f32,
    y: f32,
    text: String,
    color: Color,
    clip: Option<(i32, i32, i32, i32)>, // left, top, right, bottom
}

pub struct Text {
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    renderer: TextRenderer,
    cmds: Vec<TextCmd>,
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
        let font_system = FontSystem::new();
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
        }
    }

    /// Drop any queued text (call at the start of a frame).
    pub fn begin(&mut self) {
        self.cmds.clear();
    }

    /// Queue a text string to be drawn at (`x`, `y`) (baseline-top, in pixels).
    /// `clip` is an optional scissor rect (x, y, w, h) in pixels.
    pub fn queue(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        color: MuiColor,
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
            clip,
        });
    }

    /// Shape `text` and return its `(width, height)` extent in pixels.
    pub fn measure(&mut self, text: &str) -> (f32, f32) {
        let mut buffer = TextBuffer::new(
            &mut self.font_system,
            Metrics::new(FONT_SIZE, LINE_HEIGHT),
        );
        buffer.set_size(&mut self.font_system, None, None);
        buffer.set_text(
            &mut self.font_system,
            text,
            Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width = 0.0f32;
        let mut lines = 0u32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        let height = (lines.max(1) as f32) * LINE_HEIGHT;
        (width, height)
    }

    /// Build per-command cosmic-text buffers, prepare, and render in one pass.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<(), String> {
        if self.cmds.is_empty() {
            return Ok(());
        }

        self.viewport.update(
            queue,
            Resolution {
                width: screen_w,
                height: screen_h,
            },
        );

        // Build one shaped buffer per command.
        let mut buffers: Vec<TextBuffer> = Vec::with_capacity(self.cmds.len());
        for cmd in &self.cmds {
            let mut buffer =
                TextBuffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            buffer.set_size(&mut self.font_system, Some(screen_w as f32), Some(screen_h as f32));
            buffer.set_text(
                &mut self.font_system,
                &cmd.text,
                Attrs::new().family(Family::Monospace),
                Shaping::Advanced,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);
            buffers.push(buffer);
        }

        let areas: Vec<TextArea> = self
            .cmds
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
