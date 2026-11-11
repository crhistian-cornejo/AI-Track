//! Loading VST3 modules (`GetPluginFactory`) on Windows and macOS.
//!
//! Modules are shared between instances through a weak cache, so loading
//! the same plugin twice maps the library once. The library is unloaded
//! (after `ExitDll` / `bundleExit`) when the last instance goes away.

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use vst3::ComPtr;
use vst3::Steinberg::IPluginFactory;

use crate::error::{HostError, Result};

type GetFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

pub struct Vst3Module {
    factory: ManuallyDrop<ComPtr<IPluginFactory>>,
    path: PathBuf,
    lib: ManuallyDrop<platform::Lib>,
}

// SAFETY: the factory is a COM object (thread-safe per the VST3 spec for
// `IPluginFactory`); the library handle is only used to resolve symbols and
// to unload, both of which are thread-safe.
unsafe impl Send for Vst3Module {}
unsafe impl Sync for Vst3Module {}

impl Vst3Module {
    /// Load (or reuse) the module at `bundle` (a `.vst3` bundle directory or
    /// a single-file `.vst3` DLL).
    pub fn load(bundle: &Path) -> Result<Arc<Self>> {
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<Vst3Module>>>> = OnceLock::new();
        let key = bundle.to_path_buf();
        let cache = CACHE.get_or_init(Default::default);
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = cache.get(&key).and_then(Weak::upgrade) {
            return Ok(m);
        }
        cache.retain(|_, w| w.strong_count() > 0);
        let module = Arc::new(Self::load_uncached(bundle)?);
        cache.insert(key, Arc::downgrade(&module));
        Ok(module)
    }

    fn load_uncached(bundle: &Path) -> Result<Self> {
        let err = |reason: String| HostError::Load { path: bundle.to_path_buf(), reason };
        let lib = platform::Lib::open(bundle).map_err(err)?;
        let get_factory: GetFactoryFn = unsafe { lib.symbol("GetPluginFactory") }
            .map(|p| unsafe { std::mem::transmute::<*const std::ffi::c_void, GetFactoryFn>(p) })
            .ok_or_else(|| err("no GetPluginFactory export".into()))?;
        let raw = unsafe { get_factory() };
        let factory = unsafe { ComPtr::from_raw(raw) }.ok_or_else(|| err("GetPluginFactory returned null".into()))?;
        Ok(Self { factory: ManuallyDrop::new(factory), path: bundle.to_path_buf(), lib: ManuallyDrop::new(lib) })
    }

    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        &self.factory
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        unsafe {
            // Release the factory before the module's exit function runs.
            ManuallyDrop::drop(&mut self.factory);
            self.lib.exit();
            ManuallyDrop::drop(&mut self.lib);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::path::Path;

    use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};

    pub struct Lib {
        lib: Library,
        entered: bool,
    }

    impl Lib {
        pub fn open(bundle: &Path) -> Result<Self, String> {
            let file = crate::info::vst3_binary_path(bundle);
            if !file.exists() {
                return Err(format!("module binary not found at {}", file.display()));
            }
            // Altered search path: the plugin's own dependencies are looked
            // up next to the plugin DLL first.
            let lib = unsafe { Library::load_with_flags(&file, LOAD_WITH_ALTERED_SEARCH_PATH) }.map_err(|e| e.to_string())?;
            let mut this = Self { lib, entered: false };
            unsafe {
                if let Some(init) = this.symbol("InitDll") {
                    let init: unsafe extern "system" fn() -> bool = std::mem::transmute(init);
                    if !init() {
                        return Err("InitDll returned false".into());
                    }
                }
            }
            this.entered = true;
            Ok(this)
        }

        pub unsafe fn symbol(&self, name: &str) -> Option<*const c_void> {
            let mut cname = name.as_bytes().to_vec();
            cname.push(0);
            unsafe { self.lib.get::<*const c_void>(&cname).ok().map(|s| *s) }
        }

        pub unsafe fn exit(&self) {
            if !self.entered {
                return;
            }
            unsafe {
                if let Some(exit) = self.symbol("ExitDll") {
                    let exit: unsafe extern "system" fn() -> bool = std::mem::transmute(exit);
                    exit();
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::path::Path;

    use core_foundation::base::TCFType;
    use core_foundation::bundle::CFBundle;
    use core_foundation::string::CFString;
    use core_foundation::url::CFURL;

    pub struct Lib {
        bundle: CFBundle,
        entered: bool,
    }

    impl Lib {
        pub fn open(bundle_path: &Path) -> Result<Self, String> {
            let url = CFURL::from_path(bundle_path, true).ok_or("invalid bundle path")?;
            let bundle = CFBundle::new(url).ok_or("CFBundleCreate failed")?;
            let mut this = Self { bundle, entered: false };
            unsafe {
                let entry = this.symbol("bundleEntry").or_else(|| this.symbol("BundleEntry"));
                if let Some(entry) = entry {
                    let entry: unsafe extern "C" fn(*mut c_void) -> bool = std::mem::transmute(entry);
                    if !entry(this.bundle.as_concrete_TypeRef() as *mut c_void) {
                        return Err("bundleEntry returned false".into());
                    }
                }
            }
            this.entered = true;
            Ok(this)
        }

        pub unsafe fn symbol(&self, name: &str) -> Option<*const c_void> {
            let p = self.bundle.function_pointer_for_name(CFString::new(name));
            if p.is_null() { None } else { Some(p) }
        }

        pub unsafe fn exit(&self) {
            if !self.entered {
                return;
            }
            unsafe {
                let exit = self.symbol("bundleExit").or_else(|| self.symbol("BundleExit"));
                if let Some(exit) = exit {
                    let exit: unsafe extern "C" fn() -> bool = std::mem::transmute(exit);
                    exit();
                }
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::ffi::c_void;
    use std::path::Path;

    pub struct Lib(libloading::Library);

    impl Lib {
        pub fn open(bundle: &Path) -> Result<Self, String> {
            let file = crate::info::vst3_binary_path(bundle);
            unsafe { libloading::Library::new(file) }.map(Lib).map_err(|e| e.to_string())
        }
        pub unsafe fn symbol(&self, name: &str) -> Option<*const c_void> {
            let mut cname = name.as_bytes().to_vec();
            cname.push(0);
            unsafe { self.0.get::<*const c_void>(&cname).ok().map(|s| *s) }
        }
        pub unsafe fn exit(&self) {}
    }
}
