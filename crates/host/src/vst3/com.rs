//! Host-side VST3 COM objects: host application, component handler, plug
//! frame, memory stream, messages/attribute lists, and the real-time event
//! list and parameter-change containers used in `process`.

#![allow(non_snake_case)]

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use parking_lot::Mutex;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::com_scrape_types::Guid;
use vst3::{Class, ComRef, ComWrapper, Interface};

use crate::editor::RawWindow;
use crate::params::{ParamEdit, ParamEditKind};

// ─────────────────────────────── IHostApplication ───────────────────────────

pub(crate) struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        crate::strings::to_utf16(crate::HOST_NAME, unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(&self, cid: *mut TUID, iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let cid = unsafe { *(cid as *const Guid) };
        let unknown = if cid == IMessage::IID {
            ComWrapper::new(HostMessage::default()).to_com_ptr::<FUnknown>()
        } else if cid == IAttributeList::IID {
            ComWrapper::new(HostAttributeList::default()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        match unknown {
            Some(u) => unsafe {
                let ptr = u.as_ptr();
                ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj)
            },
            None => {
                unsafe { *obj = std::ptr::null_mut() };
                kResultFalse
            }
        }
    }
}

// ─────────────────────────────── IComponentHandler ──────────────────────────

/// Receives edits from the plugin's controller/GUI (main thread).
pub(crate) struct ComponentHandler {
    /// Towards the DAW (automation recording, undo).
    edits: Mutex<rtrb::Producer<ParamEdit>>,
    /// Towards the audio processor: GUI edits must reach `process` through
    /// `inputParameterChanges`.
    to_processor: Mutex<rtrb::Producer<(u32, f64)>>,
    pub restart_flags: AtomicI32,
    pub dirty: AtomicBool,
    pub open_requested: AtomicBool,
}

impl ComponentHandler {
    pub fn new(edits: rtrb::Producer<ParamEdit>, to_processor: rtrb::Producer<(u32, f64)>) -> Self {
        Self {
            edits: Mutex::new(edits),
            to_processor: Mutex::new(to_processor),
            restart_flags: AtomicI32::new(0),
            dirty: AtomicBool::new(false),
            open_requested: AtomicBool::new(false),
        }
    }

    fn edit(&self, id: u32, value: f32, kind: ParamEditKind) {
        if self.edits.lock().push(ParamEdit { id, value, kind }).is_err() {
            log::warn!("VST3 GUI edit queue full; dropping edit for param {id}");
        }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.edit(id, f32::NAN, ParamEditKind::Begin);
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, valueNormalized: ParamValue) -> tresult {
        let _ = self.to_processor.lock().push((id, valueNormalized));
        self.edit(id, valueNormalized as f32, ParamEditKind::Value);
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.edit(id, f32::NAN, ParamEditKind::End);
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.restart_flags.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.dirty.store(true, Ordering::Release);
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        self.open_requested.store(true, Ordering::Release);
        kResultOk
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

// ─────────────────────────────── IPlugFrame ─────────────────────────────────

/// Lets an editor view resize its host window (main thread).
pub(crate) struct PlugFrame {
    pub window: Mutex<Option<RawWindow>>,
    /// Last size we applied to (or accepted from) the view.
    pub size: Mutex<(u32, u32)>,
}

impl PlugFrame {
    pub fn new() -> Self {
        Self { window: Mutex::new(None), size: Mutex::new((0, 0)) }
    }
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, newSize: *mut ViewRect) -> tresult {
        if newSize.is_null() {
            return kInvalidArgument;
        }
        let rect = unsafe { *newSize };
        let (w, h) = ((rect.right - rect.left).max(1) as u32, (rect.bottom - rect.top).max(1) as u32);
        let Some(window) = *self.window.lock() else { return kResultFalse };
        *self.size.lock() = (w, h);
        window.set_client_size(w, h);
        // Per the VST3 protocol the host confirms with onSize.
        if let Some(view) = unsafe { ComRef::from_raw(view) } {
            let mut r = ViewRect { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            unsafe { view.onSize(&mut r) };
        }
        kResultOk
    }
}

// ─────────────────────────────── IBStream ───────────────────────────────────

/// Growable in-memory stream for `getState` / `setState`.
#[derive(Default)]
pub(crate) struct MemoryStream {
    inner: Mutex<(Vec<u8>, usize)>,
}

impl MemoryStream {
    pub fn from_bytes(data: &[u8]) -> Self {
        Self { inner: Mutex::new((data.to_vec(), 0)) }
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.inner.lock().0.clone()
    }

    pub fn rewind(&self) {
        self.inner.lock().1 = 0;
    }
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, numBytes: int32, numBytesRead: *mut int32) -> tresult {
        if buffer.is_null() || numBytes < 0 {
            return kInvalidArgument;
        }
        let mut g = self.inner.lock();
        let (data, pos) = &mut *g;
        let n = (numBytes as usize).min(data.len().saturating_sub(*pos));
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer as *mut u8, n) };
        *pos += n;
        if !numBytesRead.is_null() {
            unsafe { *numBytesRead = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, numBytes: int32, numBytesWritten: *mut int32) -> tresult {
        if buffer.is_null() || numBytes < 0 {
            return kInvalidArgument;
        }
        let mut g = self.inner.lock();
        let (data, pos) = &mut *g;
        let n = numBytes as usize;
        if data.len() < *pos + n {
            data.resize(*pos + n, 0);
        }
        unsafe { std::ptr::copy_nonoverlapping(buffer as *const u8, data.as_mut_ptr().add(*pos), n) };
        *pos += n;
        if !numBytesWritten.is_null() {
            unsafe { *numBytesWritten = n as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut g = self.inner.lock();
        let (data, cur) = &mut *g;
        let base = match mode as u32 {
            m if m == IBStream_::IStreamSeekMode_::kIBSeekSet as u32 => 0i64,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekCur as u32 => *cur as i64,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekEnd as u32 => data.len() as i64,
            _ => return kInvalidArgument,
        };
        let new = (base + pos).max(0) as usize;
        *cur = new;
        if !result.is_null() {
            unsafe { *result = new as int64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = self.inner.lock().1 as int64 };
        kResultOk
    }
}

// ─────────────────────────────── IMessage / IAttributeList ──────────────────

pub(crate) struct HostMessage {
    id: Mutex<Option<CString>>,
    attrs: ComWrapper<HostAttributeList>,
}

impl Default for HostMessage {
    fn default() -> Self {
        Self { id: Mutex::new(None), attrs: ComWrapper::new(HostAttributeList::default()) }
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.lock().as_ref().map_or(std::ptr::null(), |c| c.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        *self.id.lock() = if id.is_null() { None } else { Some(unsafe { CStr::from_ptr(id) }.to_owned()) };
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attrs.as_com_ref::<IAttributeList>().map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

#[derive(Default)]
pub(crate) struct HostAttributeList {
    map: Mutex<HashMap<CString, Attr>>,
}

unsafe fn attr_key(id: *const c_char) -> Option<CString> {
    if id.is_null() { None } else { Some(unsafe { CStr::from_ptr(id) }.to_owned()) }
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: int64) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        self.map.lock().insert(k, Attr::Int(value));
        kResultOk
    }

    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut int64) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        match self.map.lock().get(&k) {
            Some(Attr::Int(v)) if !value.is_null() => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        self.map.lock().insert(k, Attr::Float(value));
        kResultOk
    }

    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        match self.map.lock().get(&k) {
            Some(Attr::Float(v)) if !value.is_null() => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut v = Vec::new();
        let mut p = string;
        unsafe {
            while *p != 0 {
                v.push(*p);
                p = p.add(1);
            }
        }
        v.push(0);
        self.map.lock().insert(k, Attr::Str(v));
        kResultOk
    }

    unsafe fn getString(&self, id: IAttributeList_::AttrID, string: *mut TChar, sizeInBytes: uint32) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        match self.map.lock().get(&k) {
            Some(Attr::Str(v)) if !string.is_null() => {
                let cap = sizeInBytes as usize / std::mem::size_of::<TChar>();
                if cap == 0 {
                    return kResultFalse;
                }
                let n = v.len().min(cap);
                unsafe {
                    std::ptr::copy_nonoverlapping(v.as_ptr(), string, n);
                    *string.add(n - 1) = 0;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(&self, id: IAttributeList_::AttrID, data: *const c_void, sizeInBytes: uint32) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        let bytes = if data.is_null() || sizeInBytes == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(data as *const u8, sizeInBytes as usize) }.to_vec()
        };
        self.map.lock().insert(k, Attr::Bin(bytes));
        kResultOk
    }

    unsafe fn getBinary(&self, id: IAttributeList_::AttrID, data: *mut *const c_void, sizeInBytes: *mut uint32) -> tresult {
        let Some(k) = (unsafe { attr_key(id) }) else { return kInvalidArgument };
        match self.map.lock().get(&k) {
            Some(Attr::Bin(v)) if !data.is_null() && !sizeInBytes.is_null() => {
                // The pointer stays valid while the list lives and the key
                // is not overwritten, as the SDK's own implementation does.
                unsafe {
                    *data = v.as_ptr() as *const c_void;
                    *sizeInBytes = v.len() as uint32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

// ─────────────────────────────── Real-time containers ───────────────────────

pub(crate) const MAX_EVENTS: usize = 2048;
pub(crate) const MAX_PARAM_QUEUES: usize = 256;
pub(crate) const MAX_POINTS: usize = 64;

/// `IEventList` backed by a preallocated vector. Only touched by the audio
/// thread (and by the plugin during `process`).
pub(crate) struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

// SAFETY: accessed only from the audio thread that owns the enclosing
// `Vst3Audio` (during `process`, by the plugin on that same thread).
unsafe impl Send for EventList {}
unsafe impl Sync for EventList {}

impl EventList {
    pub fn new() -> Self {
        Self { events: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)) }
    }

    #[allow(clippy::mut_from_ref)]
    fn v(&self) -> &mut Vec<Event> {
        unsafe { &mut *self.events.get() }
    }

    pub fn clear(&self) {
        self.v().clear();
    }

    pub fn push(&self, e: Event) {
        let v = self.v();
        if v.len() < v.capacity() {
            v.push(e);
        }
    }
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.v().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match self.v().get(index as usize) {
            Some(ev) if !e.is_null() => {
                unsafe { *e = *ev };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        let v = self.v();
        if v.len() >= v.capacity() {
            return kResultFalse;
        }
        v.push(unsafe { *e });
        kResultOk
    }
}

pub(crate) struct ParamQueue {
    data: UnsafeCell<(u32, Vec<(i32, f64)>)>,
}

// SAFETY: see `EventList`.
unsafe impl Send for ParamQueue {}
unsafe impl Sync for ParamQueue {}

impl ParamQueue {
    fn new() -> Self {
        Self { data: UnsafeCell::new((0, Vec::with_capacity(MAX_POINTS))) }
    }

    #[allow(clippy::mut_from_ref)]
    fn d(&self) -> &mut (u32, Vec<(i32, f64)>) {
        unsafe { &mut *self.data.get() }
    }

    fn add(&self, offset: i32, value: f64) -> i32 {
        let pts = &mut self.d().1;
        // Points must be ordered by offset; replace a same-offset point.
        if let Some(last) = pts.last_mut() {
            if last.0 >= offset {
                *last = (last.0.max(offset), value);
                return pts.len() as i32 - 1;
            }
        }
        if pts.len() < pts.capacity() {
            pts.push((offset, value));
        } else if let Some(last) = pts.last_mut() {
            *last = (offset, value);
        }
        pts.len() as i32 - 1
    }

    pub fn id(&self) -> u32 {
        self.d().0
    }

    pub fn last(&self) -> Option<f64> {
        self.d().1.last().map(|p| p.1)
    }
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.d().0
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.d().1.len() as int32
    }

    unsafe fn getPoint(&self, index: int32, sampleOffset: *mut int32, value: *mut ParamValue) -> tresult {
        match self.d().1.get(index as usize) {
            Some(&(o, v)) => {
                unsafe {
                    if !sampleOffset.is_null() {
                        *sampleOffset = o;
                    }
                    if !value.is_null() {
                        *value = v;
                    }
                }
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addPoint(&self, sampleOffset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let i = self.add(sampleOffset, value);
        if !index.is_null() {
            unsafe { *index = i };
        }
        kResultOk
    }
}

/// `IParameterChanges` with a fixed pool of queues.
pub(crate) struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    ptrs: Vec<*mut IParamValueQueue>,
    used: UnsafeCell<usize>,
}

// SAFETY: see `EventList`; the raw pointers point into `queues`.
unsafe impl Send for ParamChanges {}
unsafe impl Sync for ParamChanges {}

impl ParamChanges {
    pub fn new() -> Self {
        let queues: Vec<_> = (0..MAX_PARAM_QUEUES).map(|_| ComWrapper::new(ParamQueue::new())).collect();
        let ptrs = queues.iter().map(|q| q.as_com_ref::<IParamValueQueue>().map_or(std::ptr::null_mut(), |r| r.as_ptr())).collect();
        Self { queues, ptrs, used: UnsafeCell::new(0) }
    }

    #[allow(clippy::mut_from_ref)]
    fn used(&self) -> &mut usize {
        unsafe { &mut *self.used.get() }
    }

    pub fn clear(&self) {
        for q in &self.queues[..*self.used()] {
            q.d().1.clear();
        }
        *self.used() = 0;
    }

    fn queue_for(&self, id: u32) -> Option<usize> {
        let used = self.used();
        if let Some(i) = self.queues[..*used].iter().position(|q| q.d().0 == id) {
            return Some(i);
        }
        if *used >= self.queues.len() {
            return None;
        }
        let i = *used;
        *used += 1;
        let d = self.queues[i].d();
        d.0 = id;
        d.1.clear();
        Some(i)
    }

    /// Host side: append a point (offset must be non-decreasing per id).
    pub fn add(&self, id: u32, offset: i32, value: f64) {
        if let Some(i) = self.queue_for(id) {
            self.queues[i].add(offset, value);
        }
    }

    pub fn for_each_last(&self, mut f: impl FnMut(u32, f64)) {
        for q in &self.queues[..*self.used()] {
            if let Some(v) = q.last() {
                f(q.id(), v);
            }
        }
    }
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        *self.used() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= *self.used() { std::ptr::null_mut() } else { self.ptrs[index as usize] }
    }

    unsafe fn addParameterData(&self, id: *const ParamID, index: *mut int32) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        match self.queue_for(unsafe { *id }) {
            Some(i) => {
                if !index.is_null() {
                    unsafe { *index = i as int32 };
                }
                self.ptrs[i]
            }
            None => std::ptr::null_mut(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_changes_group_by_id() {
        let pc = ParamChanges::new();
        pc.add(5, 0, 0.1);
        pc.add(9, 3, 0.2);
        pc.add(5, 10, 0.3);
        unsafe {
            assert_eq!(pc.getParameterCount(), 2);
            let q = ComRef::from_raw(pc.getParameterData(0)).unwrap();
            assert_eq!(q.getParameterId(), 5);
            assert_eq!(q.getPointCount(), 2);
            let (mut o, mut v) = (0, 0.0);
            q.getPoint(1, &mut o, &mut v);
            assert_eq!((o, v), (10, 0.3));
        }
        pc.clear();
        unsafe { assert_eq!(pc.getParameterCount(), 0) };
    }

    #[test]
    fn memory_stream_rw() {
        let s = ComWrapper::new(MemoryStream::default());
        let st = s.as_com_ref::<IBStream>().unwrap();
        let mut data = [1u8, 2, 3, 4];
        let mut n = 0;
        unsafe {
            st.write(data.as_mut_ptr() as *mut c_void, 4, &mut n);
            assert_eq!(n, 4);
            st.seek(1, IBStream_::IStreamSeekMode_::kIBSeekSet as i32, std::ptr::null_mut());
            let mut out = [0u8; 8];
            st.read(out.as_mut_ptr() as *mut c_void, 8, &mut n);
            assert_eq!(n, 3);
            assert_eq!(&out[..3], &[2, 3, 4]);
        }
    }
}
