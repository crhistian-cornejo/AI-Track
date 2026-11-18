//! The matter's GPU side: one wgpu device (Metal on macOS, DX12 on Windows)
//! with the three pipelines of `matter.wgsl`, and render targets that read
//! back to BGRA bytes. Nothing here touches GPUI or the audio thread; the
//! app drives it from its render thread (`service.rs`), the offline preview
//! and the tests call it directly.

use super::uniforms::{SIZE, Uniforms};

/// The pixel format of the panel image (what GPUI's images are).
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    matter: wgpu::RenderPipeline,
    luma: wgpu::RenderPipeline,
    chroma: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    noise: wgpu::TextureView,
    repeat: wgpu::Sampler,
    /// What the adapter calls itself (for the logs).
    pub adapter: String,
    pub backend: wgpu::Backend,
}

/// One panel's buffers: the image, its uniforms and its readback.
pub struct Target {
    pub width: u32,
    pub height: u32,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    readback: wgpu::Buffer,
    row: u32,
}

impl Gpu {
    /// Ask for an adapter and a device (blocks: call it off the UI thread).
    pub fn new() -> Result<Gpu, String> {
        pollster::block_on(Self::init(include_str!("matter.wgsl")))
    }

    /// The same with another shader source (the offline preview's live editing).
    #[allow(dead_code)]
    pub fn with_source(source: &str) -> Result<Gpu, String> {
        pollster::block_on(Self::init(source))
    }

    async fn init(source: &str) -> Result<Gpu, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::METAL | wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(desc);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::LowPower, force_fallback_adapter: false, compatible_surface: None })
            .await
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor { label: Some("bloom-matter"), required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()), ..Default::default() })
            .await
            .map_err(|e| format!("no GPU device: {e}"))?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("matter.wgsl"), source: wgpu::ShaderSource::Wgsl(source.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("matter"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(SIZE as u64) },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 4, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        // The noise lattice: one random value per cell of a 32³ tiling volume,
        // sampled trilinearly at quintic-remapped coordinates (smooth value
        // noise for one fetch instead of eight hashes).
        let noise = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("matter-noise"),
            size: wgpu::Extent3d { width: NOISE, height: NOISE, depth_or_array_layers: NOISE },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &noise, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &noise_lattice(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(NOISE), rows_per_image: Some(NOISE) },
            wgpu::Extent3d { width: NOISE, height: NOISE, depth_or_array_layers: NOISE },
        );
        let noise = noise.create_view(&Default::default());
        let repeat = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("matter-noise"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("matter"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = |entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_full"), buffers: &[], compilation_options: Default::default() },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let matter = pipeline("fs_matter", FORMAT);
        let luma = pipeline("fs_luma", wgpu::TextureFormat::R8Unorm);
        let chroma = pipeline("fs_chroma", wgpu::TextureFormat::Rg8Unorm);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("matter"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let dummy = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("matter-dummy"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Ok(Gpu { device, queue, layout, matter, luma, chroma, sampler, dummy, noise, repeat, adapter: info.name, backend: info.backend })
    }

    fn bind(&self, ubuf: &wgpu::Buffer, src: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("matter"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.noise) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.repeat) },
            ],
        })
    }

    /// A target of `width` × `height` pixels.
    pub fn target(&self, width: u32, height: u32) -> Target {
        let (width, height) = (width.clamp(2, 4096), height.clamp(2, 4096));
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("matter-panel"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let ubuf = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("matter-uniforms"), size: SIZE as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bind = self.bind(&ubuf, &self.dummy);
        let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("matter-readback"), size: u64::from(row * height), usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Target { width, height, texture, view, ubuf, bind, readback, row }
    }

    fn pass(enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline, bind: &wgpu::BindGroup) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("matter"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Record the panel of `u` into `t`.
    pub fn encode(&self, enc: &mut wgpu::CommandEncoder, t: &Target, u: &Uniforms) {
        self.queue.write_buffer(&t.ubuf, 0, &u.bytes());
        Self::pass(enc, &t.view, &self.matter, &t.bind);
    }

    /// Record the NV12 conversion of `t`'s image into the two plane views
    /// (full-size luma, half-size chroma). `t` must have been encoded first.
    pub fn encode_nv12(&self, enc: &mut wgpu::CommandEncoder, t: &Target, luma: &wgpu::TextureView, chroma: &wgpu::TextureView) {
        let bind = self.bind(&t.ubuf, &t.view);
        Self::pass(enc, luma, &self.luma, &bind);
        Self::pass(enc, chroma, &self.chroma, &bind);
    }

    /// Render `u` into `t` and read it back as tightly packed premultiplied
    /// BGRA (blocks until the GPU is done).
    pub fn render_bgra(&self, t: &Target, u: &Uniforms) -> Result<Vec<u8>, String> {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("matter") });
        self.encode(&mut enc, t, u);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &t.readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(t.row), rows_per_image: Some(t.height) } },
            wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = t.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity((t.width * t.height * 4) as usize);
        {
            let data = slice.get_mapped_range();
            for y in 0..t.height as usize {
                let start = y * t.row as usize;
                out.extend_from_slice(&data[start..start + t.width as usize * 4]);
            }
        }
        t.readback.unmap();
        Ok(out)
    }

    /// Submit recorded work and wait for it (the render thread's frames).
    pub fn submit_and_wait(&self, enc: wgpu::CommandEncoder) -> Result<(), String> {
        self.queue.submit([enc.finish()]);
        self.device.poll(wgpu::PollType::wait_indefinitely()).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Side of the noise volume (cells; it tiles).
pub const NOISE: u32 = 32;

/// The noise volume's values: a fixed hash of each cell, so every machine
/// draws the same matter.
pub fn noise_lattice() -> Vec<u8> {
    let n = NOISE as usize;
    (0..n * n * n)
        .map(|i| {
            let mut x = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((x ^ (x >> 31)) >> 56) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::frame::{Animator, Live, Panel, SOUNDS};
    use super::super::mapping::SoundLook;
    use super::super::material::Material;
    use super::*;

    /// Luminance statistics of a BGRA image: (share of near-black pixels,
    /// share of bright ones, mean).
    pub fn stats(px: &[u8]) -> (f32, f32, f32) {
        let n = (px.len() / 4) as f32;
        let (mut dark, mut bright, mut sum) = (0.0, 0.0, 0.0);
        for c in px.as_chunks::<4>().0 {
            let y = (0.0722 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.2126 * c[2] as f32) / 255.0;
            sum += y;
            if y < 0.08 {
                dark += 1.0;
            }
            if y > 0.75 {
                bright += 1.0;
            }
        }
        (dark / n, bright / n, sum / n)
    }

    fn frame(material: Material, light: bool) -> Uniforms {
        let looks = [SoundLook::default(); SOUNDS];
        let q = [0.0; SOUNDS];
        let mut a = Animator::default();
        a.step(0.016, &Live { env: &q, pulse: &q, looks: &looks, selected: 0 }, None, false);
        let bg = if light { [0.80, 0.81, 0.83] } else { [0.012, 0.012, 0.014] };
        a.uniforms(material, &Panel::fit(256.0, 256.0, (0.0, 0.0, 256.0, 256.0), 0.0, bg, light, [0.0; 3]), 0.0)
    }

    // Needs a GPU: `cargo test -p aitrack-app --bin aitrack matter -- --ignored`.
    #[test]
    #[ignore]
    fn the_mass_is_black_and_glossy_on_a_light_panel() {
        let Ok(gpu) = Gpu::new() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let t = gpu.target(256, 256);
        let px = gpu.render_bgra(&t, &frame(Material::Obsidian, true)).unwrap();
        assert_eq!(px.len(), 256 * 256 * 4);
        let (dark, bright, mean) = stats(&px);
        // A black mass covers a good part of the panel, with highlights on it,
        // on a light panel.
        assert!((0.12..0.6).contains(&dark), "dark {dark}");
        assert!(bright > 0.2, "bright {bright}");
        assert!(mean > 0.3, "mean {mean}");
        // Highlights on the mass itself: bright pixels inside the dark region's rows.
        let row = 128usize;
        let line = &px[row * 256 * 4..(row + 1) * 256 * 4];
        let (d, _, _) = stats(line);
        assert!(d > 0.2, "the middle row crosses the mass");
    }

    #[test]
    #[ignore]
    fn every_material_renders() {
        let Ok(gpu) = Gpu::new() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let t = gpu.target(128, 128);
        let mut means = Vec::new();
        for m in Material::ALL {
            let px = gpu.render_bgra(&t, &frame(m, false)).unwrap();
            let (_, _, mean) = stats(&px);
            assert!(mean.is_finite() && mean > 0.005, "{m:?}");
            means.push(mean);
        }
        // Pearl is much lighter than obsidian on a dark panel.
        assert!(means[Material::Pearl.index()] > means[Material::Obsidian.index()] * 1.5, "{means:?}");
    }
}
