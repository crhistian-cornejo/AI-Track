//! macOS: frames GPUI shows without a copy. Each frame is an IOSurface-backed
//! `CVPixelBuffer` in NV12 (what `Window::paint_surface` takes, the format
//! GPUI's video path reads); wgpu renders straight into its two planes
//! through Metal textures made from the IOSurface. Three buffers rotate, so
//! the one being written is never the one on screen.

use super::gpu::{Gpu, Target};
use super::uniforms::Uniforms;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::{CFDictionary, CFRetained, CFType};
use objc2_core_video::{CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetIOSurface, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVReturnSuccess};
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};

/// A finished frame for `paint_surface`.
#[derive(Clone)]
pub struct Frame(pub CFRetained<CVPixelBuffer>);

// SAFETY: a CVPixelBuffer is a reference-counted CoreFoundation object, safe
// to retain and release from any thread; the render thread finishes writing
// it (the GPU work is waited for) before the frame is handed over, and never
// writes it again while it can be on screen (three rotate).
unsafe impl Send for Frame {}

struct Slot {
    buffer: CFRetained<CVPixelBuffer>,
    luma: wgpu::TextureView,
    chroma: wgpu::TextureView,
    // The textures own the Metal objects the views draw into.
    _textures: [wgpu::Texture; 2],
}

/// Rotating NV12 frames of one size.
pub struct Ring {
    pub width: u32,
    pub height: u32,
    slots: Vec<Slot>,
    next: usize,
}

/// Plane `index` of `surface` (`w` × `h`) as a wgpu render target.
fn plane(gpu: &Gpu, device: &ProtocolObject<dyn MTLDevice>, surface: &IOSurfaceRef, index: usize, (w, h): (u32, u32), (mtl, format): (MTLPixelFormat, wgpu::TextureFormat)) -> Result<(wgpu::Texture, wgpu::TextureView), String> {
    // SAFETY: a plain descriptor; the IOSurface plane has exactly this size and format.
    let desc = unsafe { MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(mtl, w as usize, h as usize, false) };
    desc.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
    desc.setStorageMode(if device.hasUnifiedMemory() { MTLStorageMode::Shared } else { MTLStorageMode::Managed });
    let raw: Retained<ProtocolObject<dyn MTLTexture>> = device.newTextureWithDescriptor_iosurface_plane(&desc, surface, index).ok_or("Metal could not wrap the IOSurface")?;
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    // SAFETY: `raw` is a live 2D texture of this format and size made by the
    // same MTLDevice wgpu uses; wgpu takes a reference and only renders to it.
    let texture = unsafe {
        let hal = wgpu::hal::metal::Device::texture_from_raw(raw, format, MTLTextureType::Type2D, 1, 1, wgpu::hal::CopyExtent { width: w, height: h, depth: 1 });
        gpu.device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal,
            &wgpu::TextureDescriptor { label: Some("matter-plane"), size, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[] },
        )
    };
    let view = texture.create_view(&Default::default());
    Ok((texture, view))
}

fn slot(gpu: &Gpu, w: u32, h: u32) -> Result<Slot, String> {
    let attributes = CFDictionary::from_slices(&[unsafe { kCVPixelBufferIOSurfacePropertiesKey }], &[&*CFDictionary::<CFType, CFType>::empty()]);
    let mut out = std::ptr::null_mut();
    // SAFETY: valid size, format and attributes; `out` receives a +1 reference.
    let status = unsafe { CVPixelBufferCreate(None, w as usize, h as usize, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, Some(attributes.as_opaque()), std::ptr::NonNull::from(&mut out)) };
    if status != kCVReturnSuccess {
        return Err(format!("CVPixelBufferCreate failed ({status})"));
    }
    // SAFETY: CVPixelBufferCreate succeeded, so `out` is a new non-null buffer we own.
    let buffer = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(out).ok_or("no pixel buffer")?) };
    let surface = CVPixelBufferGetIOSurface(Some(&buffer)).ok_or("the pixel buffer has no IOSurface")?;
    // SAFETY: only reads the raw MTLDevice of a Metal-backed wgpu device.
    let hal = unsafe { gpu.device.as_hal::<wgpu::hal::api::Metal>() }.ok_or("not a Metal device")?;
    let device = hal.raw_device();
    let (lt, luma) = plane(gpu, device, &surface, 0, (w, h), (MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm))?;
    let (ct, chroma) = plane(gpu, device, &surface, 1, (w / 2, h / 2), (MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm))?;
    Ok(Slot { buffer, luma, chroma, _textures: [lt, ct] })
}

impl Ring {
    /// Three frames of `width` × `height` (rounded down to even sizes).
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Result<Ring, String> {
        let (w, h) = ((width.max(2)) & !1, (height.max(2)) & !1);
        let slots = (0..3).map(|_| slot(gpu, w, h)).collect::<Result<Vec<_>, _>>()?;
        Ok(Ring { width: w, height: h, slots, next: 0 })
    }

    /// The even size a ring for `width` × `height` has.
    pub fn size_for(width: u32, height: u32) -> (u32, u32) {
        ((width.max(2)) & !1, (height.max(2)) & !1)
    }

    /// Render `u` through `target` (same size) into the next frame and wait for it.
    pub fn render(&mut self, gpu: &Gpu, target: &Target, u: &Uniforms) -> Result<Frame, String> {
        let s = &self.slots[self.next];
        self.next = (self.next + 1) % self.slots.len();
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("matter-nv12") });
        gpu.encode(&mut enc, target, u);
        gpu.encode_nv12(&mut enc, target, &s.luma, &s.chroma);
        gpu.submit_and_wait(enc)?;
        Ok(Frame(s.buffer.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_video::{CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress};

    // Needs a GPU: `cargo test -p aitrack-app --bin aitrack surface -- --ignored`.
    #[test]
    #[ignore]
    fn frames_land_in_the_iosurface_as_nv12() {
        let Ok(gpu) = Gpu::new() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let mut ring = Ring::new(&gpu, 64, 48).unwrap();
        let target = gpu.target(64, 48);
        // A plain panel: no mass (size 0 keeps the mass out of view), grey background.
        let u = Uniforms { res: [64.0, 48.0, 0.0, 0.0], frame: [32.0, 24.0, 24.0, 0.0], bg: [0.2, 0.2, 0.2, 1.0], shape0: [0.0001, 0.0, 1.0, 0.0], ..Default::default() };
        let f = ring.render(&gpu, &target, &u).unwrap();
        unsafe {
            assert_eq!(CVPixelBufferLockBaseAddress(&f.0, CVPixelBufferLockFlags::ReadOnly), kCVReturnSuccess);
            let y = CVPixelBufferGetBaseAddressOfPlane(&f.0, 0).cast::<u8>();
            let c = CVPixelBufferGetBaseAddressOfPlane(&f.0, 1).cast::<u8>();
            let (yr, cr) = (CVPixelBufferGetBytesPerRowOfPlane(&f.0, 0), CVPixelBufferGetBytesPerRowOfPlane(&f.0, 1));
            // Linear 0.2 is about 124/255 encoded: grey luma, neutral chroma.
            let luma = *y.add(yr * 2 + 2);
            let (cb, crv) = (*c.add(cr + 2), *c.add(cr + 3));
            CVPixelBufferUnlockBaseAddress(&f.0, CVPixelBufferLockFlags::ReadOnly);
            assert!((luma as i32 - 124).abs() < 12, "luma {luma}");
            assert!((cb as i32 - 128).abs() < 4 && (crv as i32 - 128).abs() < 4, "chroma {cb} {crv}");
        }
    }
}
