//! Setup shared by the helper processes (`aitrack-plugin-scanner`,
//! `aitrack-plugin-host`).
//!
//! On macOS the helpers live in `AI-Track.app/Contents/MacOS`, so the system
//! gives them the app's identity: the first time a plugin touches AppKit, each
//! helper becomes a regular app with the AI-Track icon in the Dock. A scan runs
//! several scanners at once, and a plugin that shows a dialog while it loads
//! (a missing licence or support file) then shows several "AI-Track"s, each
//! with its dialog, each blocked until the scan timeout kills it.
//!
//! The scanner is made a background-only process and never shows a dialog: an
//! `NSAlert` returns its first button at once and its text is kept for the scan
//! report ([`take_alerts`]). The plugin host keeps its windows (plugin editors)
//! but stays out of the Dock.

use std::sync::Mutex;

/// Which helper is starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Scanner,
    PluginHost,
}

/// What plugins tried to show in dialogs in this process.
static ALERTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Call first thing in a helper's `main`, on the main thread.
pub fn init(role: Role) {
    crate::sandbox::disable_crash_dialogs();
    #[cfg(target_os = "macos")]
    macos::init(role);
    #[cfg(not(target_os = "macos"))]
    let _ = role;
}

/// The dialog texts recorded so far, oldest first; empties the list.
pub fn take_alerts() -> Vec<String> {
    std::mem::take(&mut *ALERTS.lock().unwrap_or_else(|p| p.into_inner()))
}

fn record_alert(text: String) {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() {
        ALERTS.lock().unwrap_or_else(|p| p.into_inner()).push(text);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::Role;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSString;

    /// `NSAlertFirstButtonReturn`.
    const FIRST_BUTTON: isize = 1000;

    pub(super) fn init(role: Role) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let policy = match role {
            // No Dock icon, no menu bar, never active.
            Role::Scanner => NSApplicationActivationPolicy::Prohibited,
            // No Dock icon, but editor windows can still take the keyboard.
            Role::PluginHost => NSApplicationActivationPolicy::Accessory,
        };
        NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
        if role == Role::Scanner {
            silence_alerts();
        }
    }

    /// Replace `-[NSAlert runModal]` in this process: record the text, answer
    /// with the first button, show nothing.
    fn silence_alerts() {
        let Some(class) = AnyClass::get(c"NSAlert") else { return };
        // SAFETY: the replacement has the signature of `-runModal`
        // (`NSModalResponse (id, SEL)`, type encoding `q@:`).
        unsafe {
            let imp: Imp = std::mem::transmute(run_modal as extern "C-unwind" fn(*mut AnyObject, Sel) -> isize);
            objc2::ffi::class_replaceMethod(class as *const AnyClass as *mut AnyClass, sel!(runModal), imp, c"q@:".as_ptr());
        }
    }

    extern "C-unwind" fn run_modal(alert: *mut AnyObject, _: Sel) -> isize {
        if let Some(alert) = unsafe { alert.as_ref() } {
            let title: Option<Retained<NSString>> = unsafe { msg_send![alert, messageText] };
            let detail: Option<Retained<NSString>> = unsafe { msg_send![alert, informativeText] };
            let text = [title, detail].into_iter().flatten().map(|s| s.to_string()).collect::<Vec<_>>().join(" ");
            super::record_alert(text);
        }
        FIRST_BUTTON
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_are_trimmed_and_taken_once() {
        record_alert("  One of the files this plug-in needs\ncannot be found.  ".into());
        record_alert("   ".into());
        assert_eq!(take_alerts(), vec!["One of the files this plug-in needs cannot be found.".to_owned()]);
        assert!(take_alerts().is_empty());
    }
}
