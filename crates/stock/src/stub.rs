//! Pass-through processor used while a plugin is being written.

use aitrack_dsp::{Category, PluginInfo, ProcessContext, Processor};

struct Stub {
    info: PluginInfo,
}

impl Processor for Stub {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        if self.info.category == Category::Instrument {
            ctx.buffer.clear();
        }
    }
    fn set_param(&mut self, _id: u32, _normalized: f32) {}
    fn get_param(&self, _id: u32) -> f32 {
        0.0
    }
}

#[allow(dead_code)]
pub fn stub(id: &'static str, name: &'static str, category: Category, tags: &'static [&'static str]) -> Box<dyn Processor> {
    Box::new(Stub {
        info: PluginInfo { id, name, vendor: "AI-Track", category, tags, params: Vec::new(), inputs: 2, outputs: 2, has_sidechain: false },
    })
}
