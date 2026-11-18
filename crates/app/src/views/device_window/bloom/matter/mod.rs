//! Bloom's "matter": the scene drawn as one glossy liquid mass, raymarched
//! in a WGSL shader on the GPU (`matter.wgsl`). Self-contained (std, wgpu
//! and the stock crate only), so the offline preview
//! (`examples/bloom_matter_render.rs`) builds the same code the app runs.
//!
//! * `material`: the materials (Obsidian, Mercury, Oil Slick…) as uniform sets.
//! * `mapping`: a sound's timbre → its look (film, vapour, spikes, surface).
//! * `frame`: the motion between frames and the uniforms of each.
//! * `gpu`: the device, the pipelines, render targets and readback.
//! * `service`: the app's render thread (one shared device, lazily created).
//! * `surface`: macOS only, IOSurface-backed NV12 frames GPUI shows zero-copy.

pub mod frame;
pub mod gpu;
pub mod mapping;
pub mod material;
pub mod service;
#[cfg(target_os = "macos")]
pub mod surface;
pub mod uniforms;

pub use frame::Animator;
pub use material::Material;
