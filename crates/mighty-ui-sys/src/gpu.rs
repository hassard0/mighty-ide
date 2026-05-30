//! wgpu device/surface/queue plus the solid-rect pipeline.
//!
//! Two construction paths:
//! * [`Gpu::new_windowed`] — wraps a real winit surface (driven by the IDE).
//! * [`Gpu::new_offscreen`] — renders into an owned texture for headless tests.
//!
//! Rects are batched: callers push instances during a frame and the whole
//! batch is drawn with a single instanced draw call in [`Gpu::render_rects`].

use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

/// One solid-color quad instance in pixel space, fed to the rect shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RectInstance {
    /// Top-left position in pixels.
    pub pos: [f32; 2],
    /// Size in pixels.
    pub size: [f32; 2],
    /// RGBA color, 0..=1.
    pub color: [f32; 4],
}

/// Screen-size uniform used to build a pixel-space ortho projection in WGSL.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScreenUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// Where the frame is being drawn.
pub enum RenderTarget {
    /// A real swapchain surface (windowed mode).
    Surface(wgpu::Surface<'static>),
    /// An owned texture (offscreen / headless test mode).
    Offscreen {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
    },
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub target: RenderTarget,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,

    rect_pipeline: wgpu::RenderPipeline,
    screen_buf: wgpu::Buffer,
    screen_bind_group: wgpu::BindGroup,

    /// Full-window atmospheric glow: a textured-quad pipeline sampling a once-
    /// synthesized RGBA glow image (the layered aurora radial gradients from the
    /// mockup). Drawn right after the clear, beneath everything else.
    bg_pipeline: wgpu::RenderPipeline,
    bg_bind_group: wgpu::BindGroup,
}

/// The clear color used at the start of every frame — the Aurora Noir window
/// base `#0c0e13`. Kept in sync with [`crate::theme::BG()`]. The atmospheric glow
/// is then drawn on top as a full-window textured quad ([`Gpu::render_background`]).
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0x0c as f64 / 255.0,
    g: 0x0e as f64 / 255.0,
    b: 0x13 as f64 / 255.0,
    a: 1.0,
};

/// Features Vello's compute pipeline can opt into when the adapter supports
/// them. `CLEAR_TEXTURE` is used by Vello to clear intermediate targets; it is
/// requested only if available so the rect path on weaker adapters is unaffected.
fn vello_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & wgpu::Features::CLEAR_TEXTURE
}

/// Limits Vello needs for its compute shaders. Vello's reference setup uses the
/// default (non-downlevel) limits — its storage-buffer and workgroup-storage
/// usage exceeds `downlevel_defaults`. These limits are clamped to whatever the
/// adapter actually reports so we never request more than the device allows. The
/// rect/glyphon path works fine under these (more generous) limits too.
fn vello_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    wgpu::Limits::default().using_resolution(adapter.limits())
}

fn request_adapter(
    instance: &wgpu::Instance,
    compatible_surface: Option<&wgpu::Surface<'static>>,
) -> Option<wgpu::Adapter> {
    // Try a normal adapter, then fall back to a software/fallback adapter.
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface,
        force_fallback_adapter: false,
    }))
    .or_else(|| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface,
            force_fallback_adapter: true,
        }))
    })
}

/// Pick the swapchain texture format for the windowed (surface) path.
///
/// Vello's `render_to_surface` blit step calls `ImageFormat::from_wgpu` on the
/// surface texture format, which ONLY accepts the non-sRGB `Rgba8Unorm` /
/// `Bgra8Unorm` — every other variant (incl. the sRGB `*UnormSrgb` formats)
/// hits `unimplemented!()` and panics (vello-0.3.0 `recording.rs:255`, which
/// aborts the process since `mui_end_frame` is an `extern "C"` non-unwinding
/// boundary). Vello's blit shader already emits sRGB-encoded bytes, so a UNORM
/// surface is the correct, non-double-corrected target. Prefer a Vello-supported
/// UNORM format; fall back to the first offered format only if neither is
/// present (the legacy rect/glyphon path tolerates sRGB).
fn pick_surface_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .unwrap_or_else(|| formats.first().copied().unwrap_or(wgpu::TextureFormat::Bgra8Unorm))
}

impl Gpu {
    /// Construct GPU state backed by a real window surface.
    pub fn new_windowed(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|e| format!("create_surface failed: {e}"))?;
        let adapter = request_adapter(&instance, Some(&surface))
            .ok_or_else(|| "no suitable GPU adapter found".to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("mui device"),
                // Vello-compatible features/limits (see `vello_limits`). Shared by
                // the rect path; Vello reuses this same Device/Queue.
                required_features: vello_features(&adapter),
                required_limits: vello_limits(&adapter),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .map_err(|e| format!("request_device failed: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = pick_surface_format(&caps.formats);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let (rect_pipeline, screen_buf, screen_bind_group) =
            build_rect_pipeline(&device, format, width, height);
        let (bg_pipeline, bg_bind_group) = build_background_pipeline(&device, &queue, format);

        Ok(Self {
            device,
            queue,
            target: RenderTarget::Surface(surface),
            format,
            width,
            height,
            rect_pipeline,
            screen_buf,
            screen_bind_group,
            bg_pipeline,
            bg_bind_group,
        })
    }

    /// Construct GPU state that renders into an owned offscreen texture.
    ///
    /// Returns `Ok(None)` when no GPU adapter is available at all, so callers
    /// (tests, screenshot mode) can skip gracefully rather than fail.
    pub fn new_offscreen(width: u32, height: u32) -> Result<Option<Self>, String> {
        let width = width.max(1);
        let height = height.max(1);

        let instance = wgpu::Instance::default();
        let adapter = match request_adapter(&instance, None) {
            Some(a) => a,
            None => return Ok(None),
        };
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("mui offscreen device"),
                required_features: vello_features(&adapter),
                required_limits: vello_limits(&adapter),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .map_err(|e| format!("request_device failed: {e}"))?;

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mui offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // STORAGE_BINDING lets Vello's compute pipeline render straight into
            // this texture (`Renderer::render_to_texture`); RENDER_ATTACHMENT +
            // COPY_SRC keep the rect path and PNG readback working.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());

        let (rect_pipeline, screen_buf, screen_bind_group) =
            build_rect_pipeline(&device, format, width, height);
        let (bg_pipeline, bg_bind_group) = build_background_pipeline(&device, &queue, format);

        Ok(Some(Self {
            device,
            queue,
            target: RenderTarget::Offscreen { texture, view },
            format,
            width,
            height,
            rect_pipeline,
            screen_buf,
            screen_bind_group,
            bg_pipeline,
            bg_bind_group,
        }))
    }

    /// Draw the full-window atmospheric glow quad into `view` (after the clear,
    /// before any rects). Loads (does not clear) so the `CLEAR_COLOR` base shows
    /// through the texture's transparent regions.
    pub fn render_background(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mui background pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.bg_pipeline);
        pass.set_bind_group(0, &self.bg_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Reconfigure the surface / resize the offscreen target.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        self.width = width;
        self.height = height;
        self.queue.write_buffer(
            &self.screen_buf,
            0,
            bytemuck::bytes_of(&ScreenUniform {
                size: [width as f32, height as f32],
                _pad: [0.0, 0.0],
            }),
        );
        match &mut self.target {
            RenderTarget::Surface(surface) => {
                let config = wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: self.format,
                    width,
                    height,
                    present_mode: wgpu::PresentMode::Fifo,
                    alpha_mode: wgpu::CompositeAlphaMode::Auto,
                    view_formats: vec![],
                    desired_maximum_frame_latency: 2,
                };
                surface.configure(&self.device, &config);
            }
            RenderTarget::Offscreen { texture, view } => {
                let new = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("mui offscreen target"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                });
                *view = new.create_view(&Default::default());
                *texture = new;
            }
        }
    }

    /// Draw a batch of rects into `view`, clearing first iff `clear`.
    /// `clip` is an optional scissor rect in pixels.
    pub fn render_rects(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        instances: &[RectInstance],
        clear: bool,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let instance_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mui rect instances"),
                contents: bytemuck::cast_slice(instances),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let load = if clear {
            wgpu::LoadOp::Clear(CLEAR_COLOR)
        } else {
            wgpu::LoadOp::Load
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mui rect pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        if let Some((x, y, w, h)) = clip {
            // Clamp scissor to the target bounds (wgpu validates this strictly).
            let x = x.min(self.width);
            let y = y.min(self.height);
            let w = w.min(self.width - x);
            let h = h.min(self.height - y);
            if w == 0 || h == 0 {
                return; // fully clipped: nothing to draw (pass still cleared)
            }
            pass.set_scissor_rect(x, y, w, h);
        }

        if !instances.is_empty() {
            pass.set_pipeline(&self.rect_pipeline);
            pass.set_bind_group(0, &self.screen_bind_group, &[]);
            pass.set_vertex_buffer(0, instance_buf.slice(..));
            pass.draw(0..4, 0..instances.len() as u32);
        }
    }

    /// Read back the offscreen texture as tightly-packed RGBA8 rows.
    /// Returns `None` in windowed mode.
    pub fn read_pixels(&self) -> Option<Vec<u8>> {
        let texture = match &self.target {
            RenderTarget::Offscreen { texture, .. } => texture,
            RenderTarget::Surface(_) => return None,
        };

        let bytes_per_pixel = 4u32;
        let unpadded = self.width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mui readback"),
            size: (padded * self.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mui readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);

        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buf.unmap();
        Some(out)
    }
}

fn build_rect_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::RenderPipeline, wgpu::Buffer, wgpu::BindGroup) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mui rect shader"),
        source: wgpu::ShaderSource::Wgsl(RECT_WGSL.into()),
    });

    let screen_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mui screen uniform"),
        contents: bytemuck::bytes_of(&ScreenUniform {
            size: [width as f32, height as f32],
            _pad: [0.0, 0.0],
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mui screen bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let screen_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mui screen bg"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: screen_buf.as_entire_binding(),
        }],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mui rect pl"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let instance_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<RectInstance>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            // pos
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            // size
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 8,
                shader_location: 1,
            },
            // color
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 2,
            },
        ],
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mui rect pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[instance_layout],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (pipeline, screen_buf, screen_bind_group)
}

// ---------------------------------------------------------------------------
// Atmospheric background glow (textured full-window quad)
// ---------------------------------------------------------------------------

/// Resolution of the synthesized glow texture. It is stretched to the window by
/// the fullscreen-triangle vertex shader, so a fixed mid resolution is plenty.
const BG_W: u32 = 1280;
const BG_H: u32 = 832;

/// Synthesize the Aurora Noir atmosphere as an RGBA8 image: a near-black base
/// with three layered radial glows (cool blue top-left, muted magenta top-right,
/// teal bottom) plus a faint top sheen — the same composition as the mockup
/// `body` background-image. Pure CPU math; generated once at startup.
fn synthesize_glow(w: u32, h: u32) -> Vec<u8> {
    /// One radial glow: center + radii (fractions of the window) + RGB + strength.
    struct Glow {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        r: f32,
        g: f32,
        b: f32,
        strength: f32,
    }
    // Base near-black (#0c0e13).
    let base = (0x0c as f32, 0x0e as f32, 0x13 as f32);
    let glows = [
        // cool blue, top-left  (brighter than the mockup hex so it reads through
        // the semi-opaque editor field) — toward #243a63
        Glow { cx: 0.10, cy: -0.06, rx: 0.78, ry: 0.70, r: 0x24 as f32, g: 0x3a as f32, b: 0x63 as f32, strength: 1.0 },
        // muted magenta, top-right — toward #3a2742
        Glow { cx: 1.02, cy: -0.02, rx: 0.66, ry: 0.60, r: 0x3a as f32, g: 0x27 as f32, b: 0x42 as f32, strength: 1.0 },
        // teal, bottom-center — toward #163140
        Glow { cx: 0.58, cy: 1.18, rx: 1.05, ry: 0.95, r: 0x16 as f32, g: 0x31 as f32, b: 0x40 as f32, strength: 1.0 },
    ];
    let wf = w as f32;
    let hf = h as f32;
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let fy = y as f32 / hf;
        for x in 0..w {
            let fx = x as f32 / wf;
            let (mut r, mut g, mut b) = base;
            for glow in glows.iter() {
                let dx = (fx - glow.cx) / glow.rx;
                let dy = (fy - glow.cy) / glow.ry;
                let d2 = dx * dx + dy * dy;
                // Smooth radial falloff: 1 at center -> 0 at the radius edge.
                let t = (1.0 - d2).max(0.0);
                let falloff = t * t * glow.strength;
                r += (glow.r - base.0).max(0.0) * falloff;
                g += (glow.g - base.1).max(0.0) * falloff;
                b += (glow.b - base.2).max(0.0) * falloff;
            }
            // Faint top sheen (a thin lighter band near the very top).
            let sheen = (1.0 - (fy * 9.0)).max(0.0) * 6.0;
            r += sheen;
            g += sheen;
            b += sheen;
            let i = ((y * w + x) * 4) as usize;
            out[i] = r.clamp(0.0, 255.0) as u8;
            out[i + 1] = g.clamp(0.0, 255.0) as u8;
            out[i + 2] = b.clamp(0.0, 255.0) as u8;
            out[i + 3] = 255;
        }
    }
    out
}

/// Build the background pipeline: upload the synthesized glow as a sampled
/// texture and a fullscreen-triangle pipeline that stretches it to the window.
fn build_background_pipeline(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroup) {
    let pixels = synthesize_glow(BG_W, BG_H);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mui bg glow"),
        size: wgpu::Extent3d {
            width: BG_W,
            height: BG_H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // The glow is authored in linear-ish sRGB byte values; an sRGB view keeps
        // it perceptually correct against the sRGB swapchain.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(BG_W * 4),
            rows_per_image: Some(BG_H),
        },
        wgpu::Extent3d {
            width: BG_W,
            height: BG_H,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("mui bg sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mui bg shader"),
        source: wgpu::ShaderSource::Wgsl(BG_WGSL.into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mui bg bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mui bg bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mui bg pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mui bg pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    (pipeline, bind_group)
}

const BG_WGSL: &str = r#"
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Fullscreen triangle covering the viewport.
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = pos[vid];
    var out: VsOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    // Map clip-space to [0,1] UV with y flipped (top-left origin).
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

@group(0) @binding(0) var glow_tex: texture_2d<f32>;
@group(0) @binding(1) var glow_samp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(glow_tex, glow_samp, in.uv);
}
"#;

const RECT_WGSL: &str = r#"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    // Unit quad corners for a triangle-strip (0,0)(1,0)(0,1)(1,1).
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vid];
    let px = pos + c * size;                 // pixel-space position
    // Pixel-space ortho: (0,0) top-left -> NDC (-1,+1) top-left.
    let ndc = vec2<f32>(
        px.x / screen.size.x * 2.0 - 1.0,
        1.0 - px.y / screen.size.y * 2.0,
    );
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

#[cfg(test)]
mod tests {
    use super::pick_surface_format;
    use wgpu::TextureFormat::*;

    /// The windowed surface format must be a Vello-blittable UNORM format — an
    /// sRGB choice would abort the process in `render_to_surface` (the windowed
    /// IDE crash this fix resolves). When both UNORM and sRGB are offered (the
    /// usual Windows case), the UNORM one must win regardless of order.
    #[test]
    fn windowed_format_is_unorm_not_srgb() {
        // Typical Windows surface caps: sRGB listed first.
        let caps = [Bgra8UnormSrgb, Bgra8Unorm, Rgba8Unorm, Rgba8UnormSrgb];
        let f = pick_surface_format(&caps);
        assert!(matches!(f, Bgra8Unorm | Rgba8Unorm), "got {f:?}");
        assert!(!f.is_srgb(), "must not pick an sRGB format, got {f:?}");

        // Rgba ordering / sRGB-first also resolves to a UNORM format.
        assert!(!pick_surface_format(&[Rgba8UnormSrgb, Rgba8Unorm]).is_srgb());
        assert_eq!(pick_surface_format(&[Bgra8Unorm]), Bgra8Unorm);
    }

    /// If a driver somehow offered only sRGB formats we still return *something*
    /// (the legacy rect path tolerates it); the picker must not panic on the
    /// fallback branch.
    #[test]
    fn picker_falls_back_without_panicking() {
        assert_eq!(pick_surface_format(&[Bgra8UnormSrgb]), Bgra8UnormSrgb);
        assert_eq!(pick_surface_format(&[]), Bgra8Unorm);
    }
}
