//! VST3 hosting on top of the `vst3` crate (coupler-rs bindings generated
//! from the MIT-licensed VST3 SDK 3.8).
//!
//! - [`module`]: loading `.vst3` bundles and `GetPluginFactory`.
//! - `com`: host-side COM objects (IHostApplication, IComponentHandler,
//!   IPlugFrame, IBStream, IMessage, IEventList, IParameterChanges).
//! - `instance`: component/controller creation (single-component and split,
//!   connected through IConnectionPoint), bus setup, `process`, state,
//!   editor.

mod com;
mod instance;
pub mod module;

pub(crate) use instance::{Vst3Audio, Vst3Main, load};
pub use module::Vst3Module;

use std::path::Path;

use vst3::Steinberg::*;
use vst3::{ComPtr, Interface};

use crate::error::Result;
use crate::info::{HostedPluginInfo, tuid_to_hex};
use crate::strings::{from_c_chars, from_utf16};
use aitrack_model::PluginFormat;

/// The VST3 category of effect/instrument processors.
pub const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

#[derive(Clone, Debug)]
pub(crate) struct ClassEntry {
    pub cid: TUID,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub sub_categories: String,
}

pub(crate) fn audio_classes(factory: &ComPtr<IPluginFactory>) -> Vec<ClassEntry> {
    let mut factory_vendor = String::new();
    unsafe {
        let mut fi: PFactoryInfo = std::mem::zeroed();
        if factory.getFactoryInfo(&mut fi) == vst3::Steinberg::kResultOk {
            factory_vendor = from_c_chars(&fi.vendor);
        }
    }
    let f2 = factory.cast::<IPluginFactory2>();
    let f3 = factory.cast::<IPluginFactory3>();
    let count = unsafe { factory.countClasses() };
    let mut out = Vec::new();
    for i in 0..count {
        let mut basic: PClassInfo = unsafe { std::mem::zeroed() };
        if unsafe { factory.getClassInfo(i, &mut basic) } != vst3::Steinberg::kResultOk {
            continue;
        }
        if from_c_chars(&basic.category) != AUDIO_MODULE_CLASS {
            continue;
        }
        let mut entry = ClassEntry {
            cid: basic.cid,
            name: from_c_chars(&basic.name),
            vendor: factory_vendor.clone(),
            version: String::new(),
            sub_categories: String::new(),
        };
        let mut got = false;
        if let Some(f3) = &f3 {
            let mut w: PClassInfoW = unsafe { std::mem::zeroed() };
            if unsafe { f3.getClassInfoUnicode(i, &mut w) } == vst3::Steinberg::kResultOk {
                entry.name = from_utf16(&w.name);
                let v = from_utf16(&w.vendor);
                if !v.is_empty() {
                    entry.vendor = v;
                }
                entry.version = from_utf16(&w.version);
                entry.sub_categories = from_c_chars(&w.subCategories);
                got = true;
            }
        }
        if !got {
            if let Some(f2) = &f2 {
                let mut c2: PClassInfo2 = unsafe { std::mem::zeroed() };
                if unsafe { f2.getClassInfo2(i, &mut c2) } == vst3::Steinberg::kResultOk {
                    entry.name = from_c_chars(&c2.name);
                    let v = from_c_chars(&c2.vendor);
                    if !v.is_empty() {
                        entry.vendor = v;
                    }
                    entry.version = from_c_chars(&c2.version);
                    entry.sub_categories = from_c_chars(&c2.subCategories);
                }
            }
        }
        out.push(entry);
    }
    out
}

pub(crate) fn entry_to_info(path: &Path, e: &ClassEntry) -> HostedPluginInfo {
    let cid: [u8; 16] = e.cid.map(|b| b as u8);
    let is_instrument = e.sub_categories.split('|').any(|c| c.eq_ignore_ascii_case("Instrument"));
    HostedPluginInfo {
        format: PluginFormat::Vst3,
        uid: tuid_to_hex(&cid),
        name: e.name.clone(),
        vendor: e.vendor.clone(),
        version: e.version.clone(),
        category: e.sub_categories.clone(),
        is_instrument,
        path: path.to_path_buf(),
        inputs: if is_instrument { 0 } else { 2 },
        outputs: 2,
        params: 0,
        probed: false,
    }
}

/// List the processor classes of a module (loads the module, does not
/// instantiate plugins).
pub fn enumerate(path: &Path) -> Result<Vec<HostedPluginInfo>> {
    let module = Vst3Module::load(path)?;
    Ok(audio_classes(module.factory()).iter().map(|e| entry_to_info(path, e)).collect())
}

/// Like [`enumerate`], then instantiates each class to read its real bus
/// and parameter counts. Used by the out-of-process scanner.
pub fn probe(path: &Path) -> Result<Vec<HostedPluginInfo>> {
    let mut infos = enumerate(path)?;
    for info in &mut infos {
        match instance::probe(info) {
            Ok((inputs, outputs, params)) => {
                info.inputs = inputs;
                info.outputs = outputs;
                info.params = params;
                info.probed = true;
            }
            Err(e) => log::warn!("VST3 probe of {} failed: {e}", info.name),
        }
    }
    Ok(infos)
}

pub(crate) fn iid_ptr<I: Interface>() -> *const std::ffi::c_char {
    I::IID.as_ptr() as *const std::ffi::c_char
}
