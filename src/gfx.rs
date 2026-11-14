//! Instanced wgpu renderer for the pixel-field tiles (treemap, drive picker) and particles,
//! injected into egui's render pass via a paint callback.
//!
//! Every dot cell is one colour, so each draw is rendered at one texel per cell into its own
//! texture first, then scaled up cell by cell onto the screen: at 4K the heavy shader runs on
//! 1/9-1/16 of the pixels.

use eframe::egui;
use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
/// 28 bytes: a draw can hold ~100k of them, uploaded every frame of a transition.
pub struct Inst {
    pub rect: [f32; 4],
    /// rgb, intensity (particles: level / 4); 0..255 = 0..1.
    pub color: [u8; 4],
    /// Kind (FIELD / PARTICLE) | SOLID | OPEN; label box (index into the draw's `boxes`, NO_LABEL:
    /// none); first and last transition layer it belongs to.
    pub p: [u8; 4],
    /// Hover, tone, label visibility (scales the label box's dimming), unused; 0..255 = 0..1.
    pub q: [u8; 4],
}

/// In its folder's block of small tiles: no gutter of its own.
pub const SOLID: u8 = 4;
/// Open folder: its top `layout::header` is the title strip, the rest its dark body.
pub const OPEN: u8 = 8;
/// No label box; also how many boxes a draw can carry.
pub const NO_LABEL: u8 = 255;

/// 0..1 as 0..255.
pub fn unorm(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A palette colour and intensity (0..1) as an instance's `color`.
pub fn rgba([r, g, b]: [u8; 3], a: f32) -> [u8; 4] {
    [r, g, b, if a >= 1.0 { 255 } else { unorm(a) }]
}

/// One transition wave: (origin.xy, max dist, progress 0..1), wave rect, (outside the rect is new: 0/1).
pub type Wave = [[f32; 4]; 3];
/// Waves one draw can carry (a transition stack of up to MAX_WAVES + 1 layers).
pub const MAX_WAVES: usize = 4;

pub const FIELD: u8 = 0;
pub const PARTICLE: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    origin: [f32; 2],
    size: [f32; 2],
    ppp: f32,
    nw: f32,
    /// First dot cell of the draw's texture (screen cells).
    cell0: [f32; 2],
    /// Title strip height (pt) of OPEN tiles.
    header: f32,
    _pad: [f32; 3],
    waves: [Wave; MAX_WAVES],
    // Followed in the buffer by NO_LABEL label boxes (see `Draw::boxes`).
}

// Layouts shader.wgsl relies on: the boxes start right after U's 15 vec4s.
const _: () = assert!(size_of::<Inst>() == 28 && size_of::<Uniform>() == 15 * 16);

/// Uniform buffer size: `Uniform`, then the label boxes.
const UBUF: usize = size_of::<Uniform>() + NO_LABEL as usize * 16;

/// Each callback in a frame gets its own slot so their buffers don't overwrite each other.
struct Slot {
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    ibuf: wgpu::Buffer,
    cap: usize,
    count: u32,
    /// The draw at one texel per dot cell, its size in cells, and its bind group for the blit.
    tex: Option<(wgpu::TextureView, [u32; 2], wgpu::BindGroup)>,
}

struct Res {
    pipeline: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    tbgl: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    slots: Vec<Slot>,
}

fn slot(dev: &wgpu::Device, bgl: &wgpu::BindGroupLayout) -> Slot {
    let ubuf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: &[0; UBUF],
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: bgl,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() }],
    });
    let cap = 1024;
    Slot { ubuf, bind, ibuf: inst_buf(dev, cap), cap, count: 0, tex: None }
}

pub fn init(rs: &RenderState) {
    let dev = &rs.device;
    let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pixel"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
    });
    let bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let tbgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let target = Some(wgpu::ColorTargetState {
        format: rs.target_format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    });
    let blit = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blit"),
        layout: Some(&dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl), Some(&tbgl)],
            immediate_size: 0,
        })),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_blit"), compilation_options: Default::default(), buffers: &[] },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_blit"),
            compilation_options: Default::default(),
            targets: std::slice::from_ref(&target),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let pipeline = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pixel"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<Inst>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Unorm8x4, 2 => Uint8x4, 3 => Unorm8x4],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[target],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    rs.renderer.write().callback_resources.insert(Res { pipeline, blit, bgl, tbgl, format: rs.target_format, slots: vec![] });
}

fn inst_buf(dev: &wgpu::Device, cap: usize) -> wgpu::Buffer {
    dev.create_buffer(&wgpu::BufferDescriptor {
        label: Some("inst"),
        size: (cap * size_of::<Inst>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

struct Draw {
    insts: Vec<Inst>,
    /// Label boxes (screen min.xy, max.xy), at most NO_LABEL: the dots under and around each stay dim.
    boxes: Vec<[f32; 4]>,
    rect: egui::Rect,
    slot: usize,
    waves: [Wave; MAX_WAVES],
    nw: usize,
}

impl CallbackTrait for Draw {
    fn prepare(
        &self,
        dev: &wgpu::Device,
        queue: &wgpu::Queue,
        sd: &ScreenDescriptor,
        enc: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let _s = crate::perf::span("prev-upload");
        crate::perf::add("#prev-upload-kb", (self.insts.len() * size_of::<Inst>()) as f64 / 1024.0);
        let res: &mut Res = res.get_mut().unwrap();
        while res.slots.len() <= self.slot {
            res.slots.push(slot(dev, &res.bgl));
        }
        let r = &mut res.slots[self.slot];
        r.count = self.insts.len() as u32;
        if r.count == 0 {
            return vec![];
        }
        if self.insts.len() > r.cap {
            r.cap = self.insts.len().next_power_of_two();
            r.ibuf = inst_buf(dev, r.cap);
        }
        queue.write_buffer(&r.ibuf, 0, bytemuck::cast_slice(&self.insts));
        // The dot cells the rect touches (cell size as in shader.wgsl).
        let ppp = sd.pixels_per_point;
        let cs = (2.0 * ppp).floor().max(2.0);
        let c0 = (self.rect.min.to_vec2() * ppp / cs).floor();
        let c1 = (self.rect.max.to_vec2() * ppp / cs).ceil();
        let n = [(c1.x - c0.x).max(1.0) as u32, (c1.y - c0.y).max(1.0) as u32];
        let u = Uniform {
            origin: (c0 * cs / ppp).into(),
            size: (egui::vec2(n[0] as f32, n[1] as f32) * cs / ppp).into(),
            ppp,
            nw: self.nw as f32,
            cell0: c0.into(),
            header: crate::layout::header(crate::layout::grid(ppp)),
            _pad: [0.0; 3],
            waves: self.waves,
        };
        queue.write_buffer(&r.ubuf, 0, bytemuck::bytes_of(&u));
        if !self.boxes.is_empty() {
            queue.write_buffer(&r.ubuf, size_of::<Uniform>() as u64, bytemuck::cast_slice(&self.boxes[..self.boxes.len().min(NO_LABEL as usize)]));
        }
        if r.tex.as_ref().is_none_or(|t| t.1 != n) {
            let tex = dev.create_texture(&wgpu::TextureDescriptor {
                label: Some("field"),
                size: wgpu::Extent3d { width: n[0], height: n[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: res.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &res.tbgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
            });
            r.tex = Some((view, n, bind));
        }
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("field"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &r.tex.as_ref().unwrap().0,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &r.bind, &[]);
        pass.set_vertex_buffer(0, r.ibuf.slice(..));
        pass.draw(0..6, 0..r.count);
        vec![]
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, res: &CallbackResources) {
        let res: &Res = res.get().unwrap();
        let Some(r) = res.slots.get(self.slot).filter(|r| r.count > 0) else { return };
        let Some((_, _, tex)) = &r.tex else { return };
        pass.set_pipeline(&res.blit);
        pass.set_bind_group(0, &r.bind, &[]);
        pass.set_bind_group(1, tex, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// `slot` must be unique per callback within a frame. The painter's clip rect becomes the scissor.
pub fn paint(painter: &egui::Painter, rect: egui::Rect, insts: Vec<Inst>, slot: usize, waves: &[Wave]) {
    painter.add(shape(rect, insts, vec![], slot, waves));
}

/// The paint callback as a shape, for callers that reserve its place in the paint order first.
/// `waves`: at most MAX_WAVES, oldest first; layer j is shown where wave j-1 is the newest one passed.
/// `boxes`: label boxes the instances' `p[1]` index.
pub fn shape(rect: egui::Rect, insts: Vec<Inst>, boxes: Vec<[f32; 4]>, slot: usize, waves: &[Wave]) -> egui::Shape {
    let mut w = [[[0.0; 4]; 3]; MAX_WAVES];
    w[..waves.len()].copy_from_slice(waves);
    egui_wgpu::Callback::new_paint_callback(rect, Draw { insts, boxes, rect, slot, waves: w, nw: waves.len() }).into()
}
