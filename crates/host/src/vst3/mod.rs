//! VST3 hosting on top of the `vst3` crate (coupler-rs bindings generated
//! from the MIT-licensed VST3 SDK 3.8).
//!
//! - [`module`]: loading `.vst3` bundles and `GetPluginFactory`.
//! - `com`: host-side COM objects (IHostApplication, IComponentHandler,
//!   IPlugFrame, IBStream, IMessage, IEventList, IParameterChanges).
//! - `instance`: component/controller creation (single-component and split,
//!   connected through IConnectionPoint), bus setup, `process`, state,
//!   editor.

// The SDK's enum constants are `c_int` on Windows and `c_uint` elsewhere:
// casts that are no-ops on one platform are required on the other.
#![allow(clippy::unnecessary_cast)]

mod com;
mod instance;
pub mod module;

pub(crate) use instance::{Vst3Audio, Vst3Main, load};
pub use module::Vst3Module;

use std::path::Path;

use vst3::Steinberg::*;
use vst3::{ComPtr, Interface};

use crate::error::Result;
use crate::info::HostedPluginInfo;
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
        if !got && let Some(f2) = &f2 {
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
        out.push(entry);
    }
    out
}

/// A class id as 32 hex digits of its four 32-bit words (the SDK's
/// `INLINE_UID` / moduleinfo.json form). Identical on every platform, even
/// though Windows stores the bytes COM-swapped in memory.
pub fn uid_to_string(cid: &TUID) -> String {
    let b: [u8; 16] = cid.map(|x| x as u8);
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (a, w1) = if cfg!(windows) {
        let a = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let hi = u16::from_le_bytes([b[4], b[5]]) as u32;
        let lo = u16::from_le_bytes([b[6], b[7]]) as u32;
        (a, (hi << 16) | lo)
    } else {
        (be(0), be(4))
    };
    format!("{a:08X}{w1:08X}{:08X}{:08X}", be(8), be(12))
}

/// Inverse of [`uid_to_string`].
pub fn uid_from_string(s: &str) -> Option<TUID> {
    let bytes = tuid_hex(s)?;
    let w = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Some(vst3::uid(w(0), w(4), w(8), w(12)))
}

fn tuid_hex(s: &str) -> Option<[u8; 16]> {
    crate::info::hex_to_tuid(s)
}

pub(crate) fn entry_to_info(path: &Path, e: &ClassEntry) -> HostedPluginInfo {
    let is_instrument = e.sub_categories.split('|').any(|c| c.eq_ignore_ascii_case("Instrument"));
    HostedPluginInfo {
        format: PluginFormat::Vst3,
        uid: uid_to_string(&e.cid),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_string_is_platform_independent() {
        let cid = vst3::uid(0x11223344, 0x55667788, 0x99AABBCC, 0xDDEEFF00);
        assert_eq!(uid_to_string(&cid), "112233445566778899AABBCCDDEEFF00");
        assert_eq!(uid_from_string("112233445566778899aabbccddeeff00"), Some(cid));
    }
}
