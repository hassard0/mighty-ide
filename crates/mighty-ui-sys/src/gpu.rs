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
}

/// The clear color used at the start of every frame (dark editor background).
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.08,
    g: 0.08,
    b: 0.10,
    a: 1.0,
};

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
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .map_err(|e| format!("request_device failed: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
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
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());

        let (rect_pipeline, screen_buf, screen_bind_group) =
            build_rect_pipeline(&device, format, width, height);

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
        }))
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
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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
