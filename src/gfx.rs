//! Instanced wgpu renderer for the pixel-field tiles (treemap, drive picker) and particles,
//! injected into egui's render pass via a paint callback.

use bytemuck::Zeroable;
use eframe::egui;
use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Inst {
    pub rect: [f32; 4],
    pub color: [f32; 4],
    pub p: [f32; 4],
    pub q: [f32; 4],
    /// x = transition role (STATIC / INCOMING / OUTGOING), y = hover tone equalisation; zw unused.
    pub o: [f32; 4],
    /// Label box (screen min.xy, max.xy): dots under and around it stay dim. NO_TEXT = none.
    pub t: [f32; 4],
}

pub const NO_TEXT: [f32; 4] = [0.0, 0.0, -1.0, -1.0];

pub const STATIC: [f32; 4] = [0.0; 4];
pub const INCOMING: f32 = 1.0;
pub const OUTGOING: f32 = 2.0;
/// Wave uniform (origin.xy, max dist, K), wave rect, (progress outside rect). K > 1: settled.
pub type Wave = [[f32; 4]; 3];
pub const NO_WAVE: Wave = [[0.0, 0.0, 1.0, 2.0], [0.0; 4], [1.0, 0.0, 0.0, 0.0]];

pub const FIELD: f32 = 0.0;
pub const PARTICLE: f32 = 2.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    origin: [f32; 2],
    size: [f32; 2],
    ppp: f32,
    time: f32,
    _pad: [f32; 2],
    wave: Wave,
}

/// Each callback in a frame gets its own slot so their buffers don't overwrite each other.
struct Slot {
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    ibuf: wgpu::Buffer,
    cap: usize,
    count: u32,
}

struct Res {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    slots: Vec<Slot>,
}

fn slot(dev: &wgpu::Device, bgl: &wgpu::BindGroupLayout) -> Slot {
    let ubuf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&Uniform::zeroed()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: bgl,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() }],
    });
    let cap = 1024;
    Slot { ubuf, bind, ibuf: inst_buf(dev, cap), cap, count: 0 }
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
    let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
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
                attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: rs.target_format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    rs.renderer.write().callback_resources.insert(Res { pipeline, bgl, slots: vec![] });
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
    rect: egui::Rect,
    time: f32,
    slot: usize,
    wave: Wave,
}

impl CallbackTrait for Draw {
    fn prepare(
        &self,
        dev: &wgpu::Device,
        queue: &wgpu::Queue,
        sd: &ScreenDescriptor,
        _enc: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &mut Res = res.get_mut().unwrap();
        while res.slots.len() <= self.slot {
            res.slots.push(slot(dev, &res.bgl));
        }
        let r = &mut res.slots[self.slot];
        if self.insts.len() > r.cap {
            r.cap = self.insts.len().next_power_of_two();
            r.ibuf = inst_buf(dev, r.cap);
        }
        queue.write_buffer(&r.ibuf, 0, bytemuck::cast_slice(&self.insts));
        let u = Uniform {
            origin: self.rect.min.into(),
            size: self.rect.size().into(),
            ppp: sd.pixels_per_point,
            time: self.time,
            _pad: [0.0; 2],
            wave: self.wave,
        };
        queue.write_buffer(&r.ubuf, 0, bytemuck::bytes_of(&u));
        r.count = self.insts.len() as u32;
        vec![]
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, res: &CallbackResources) {
        let res: &Res = res.get().unwrap();
        let Some(r) = res.slots.get(self.slot).filter(|r| r.count > 0) else { return };
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &r.bind, &[]);
        pass.set_vertex_buffer(0, r.ibuf.slice(..));
        pass.draw(0..6, 0..r.count);
    }
}

/// `slot` must be unique per callback within a frame. The painter's clip rect becomes the scissor.
pub fn paint(painter: &egui::Painter, rect: egui::Rect, insts: Vec<Inst>, time: f32, slot: usize, wave: Wave) {
    painter.add(shape(rect, insts, time, slot, wave));
}

/// The paint callback as a shape, for callers that reserve its place in the paint order first.
pub fn shape(rect: egui::Rect, insts: Vec<Inst>, time: f32, slot: usize, wave: Wave) -> egui::Shape {
    egui_wgpu::Callback::new_paint_callback(rect, Draw { insts, rect, time, slot, wave }).into()
}
