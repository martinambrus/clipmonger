//! `AppKit` glue (macOS only): `NSPasteboard`, `NSWorkspace`,
//! `NSRunningApplication`, `NSBitmapImageRep`. The platform-neutral modules
//! call these through `crate::os`; `stub.rs` mirrors every signature.
//!
//! Unsafe is confined to Objective-C sends the generated bindings do not
//! cover (nil-able `generalPasteboard`, OS-version-gated selectors) and to
//! reading framework constants.
#![allow(unsafe_code)]

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{NSObjectProtocol as _, ProtocolObject};
use objc2::{msg_send, sel, ClassType as _};
use objc2_app_kit::{
    NSApplicationActivationOptions, NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard,
    NSPasteboardAccessBehavior, NSPasteboardItem, NSPasteboardTypeHTML, NSPasteboardTypePNG,
    NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardWriting,
    NSRunningApplication, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSNumber, NSString, NSUTF8StringEncoding, NSURL,
};

use clipmonger_platform::types::{Error, Result};

use crate::caps::PasteboardAccess;
use crate::foreground::RunningApp;
use crate::mime::{Item, TypeNames, WritePlan};
use crate::sys;

pub(crate) use crate::sys::{held_modifier_flags, post_key, post_unicode, secure_input_enabled};

/// Refusal when there is no per-user pasteboard (system daemon context).
const NO_SESSION: &str = "no user pasteboard: Clipmonger must run in the logged-in user's \
     session (LaunchAgent), not as a system daemon";

fn general() -> Result<Retained<NSPasteboard>> {
    // Safety: class method without arguments; nil under launchd daemon
    // contexts (seg-08), hence the optional return.
    let pb: Option<Retained<NSPasteboard>> =
        unsafe { msg_send![NSPasteboard::class(), generalPasteboard] };
    pb.ok_or_else(|| Error::Unavailable(NO_SESSION.to_string()))
}

/// Borrow a bridged `CFStringRef` constant as `NSString`.
fn bridged(s: sys::CFStringRef) -> Result<String> {
    if s.is_null() {
        return Err(Error::Unavailable("missing UTI constant".to_string()));
    }
    // Safety: CFString is toll-free bridged to NSString; constants live forever.
    let s: &NSString = unsafe { &*s.cast::<NSString>() };
    Ok(s.to_string())
}

pub(crate) fn type_names() -> Result<TypeNames> {
    general()?;
    // Safety: AppKit constants (10.6+), immutable.
    let (string, html, rtf, png, tiff) = unsafe {
        (
            NSPasteboardTypeString.to_string(),
            NSPasteboardTypeHTML.to_string(),
            NSPasteboardTypeRTF.to_string(),
            NSPasteboardTypePNG.to_string(),
            NSPasteboardTypeTIFF.to_string(),
        )
    };
    Ok(TypeNames {
        string,
        utf16: bridged(sys::uti_utf16())?,
        html,
        rtf,
        png,
        tiff,
        file_url: bridged(sys::uti_file_url())?,
    })
}

pub(crate) fn change_count() -> Result<isize> {
    autoreleasepool(|_| Ok(general()?.changeCount()))
}

fn strings(array: &NSArray<NSString>) -> Vec<String> {
    array.iter().map(|s| s.to_string()).collect()
}

pub(crate) fn types() -> Result<Vec<String>> {
    autoreleasepool(|_| {
        Ok(general()?
            .types()
            .map(|array| strings(&array))
            .unwrap_or_default())
    })
}

fn too_large(len: usize, max: u64) -> Result<()> {
    if len as u64 > max {
        return Err(Error::TooLarge(format!(
            "pasteboard data is {len} bytes, limit {max}"
        )));
    }
    Ok(())
}

pub(crate) fn data(uti: &str, max: u64) -> Result<Option<Vec<u8>>> {
    autoreleasepool(|_| {
        let pb = general()?;
        let Some(data) = pb.dataForType(&NSString::from_str(uti)) else {
            return Ok(None);
        };
        too_large(data.len(), max)?;
        Ok(Some(data.to_vec()))
    })
}

pub(crate) fn string_utf8(max: u64) -> Result<Option<Vec<u8>>> {
    autoreleasepool(|_| {
        let pb = general()?;
        // Safety: AppKit constant.
        let Some(s) = pb.stringForType(unsafe { NSPasteboardTypeString }) else {
            return Ok(None);
        };
        too_large(s.lengthOfBytesUsingEncoding(NSUTF8StringEncoding), max)?;
        Ok(Some(s.to_string().into_bytes()))
    })
}

/// Absolute `file://` URL of `url`, resolving file reference URLs
/// (`file:///.file/id=...`, as Finder writes them) to path URLs.
fn file_url_string(url: Retained<NSURL>) -> Option<String> {
    let url = if url.isFileReferenceURL() {
        url.filePathURL()?
    } else {
        url
    };
    url.isFileURL()
        .then(|| url.absoluteString().map(|s| s.to_string()))
        .flatten()
}

pub(crate) fn file_urls(file_url_type: &str) -> Result<Vec<String>> {
    autoreleasepool(|_| {
        let pb = general()?;
        let kind = NSString::from_str(file_url_type);
        let Some(items) = pb.pasteboardItems() else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|item| item.stringForType(&kind))
            .filter_map(|s| NSURL::URLWithString(&s))
            .filter_map(file_url_string)
            .collect())
    })
}

pub(crate) fn snapshot(max_total: u64) -> Result<Vec<Item>> {
    autoreleasepool(|_| {
        let pb = general()?;
        let Some(items) = pb.pasteboardItems() else {
            return Ok(Vec::new());
        };
        let mut total = 0u64;
        let mut out = Vec::new();
        for item in &items {
            let mut entry = Vec::new();
            for kind in &item.types() {
                if let Some(data) = item.dataForType(&kind) {
                    total = total.saturating_add(data.len() as u64);
                    if total > max_total {
                        return Err(Error::TooLarge(
                            "clipboard too large to save for restore".to_string(),
                        ));
                    }
                    entry.push((kind.to_string(), data.to_vec()));
                }
            }
            out.push(entry);
        }
        Ok(out)
    })
}

fn item_with(data: &[(String, Vec<u8>)]) -> Retained<NSPasteboardItem> {
    let item = NSPasteboardItem::new();
    for (kind, bytes) in data {
        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind));
    }
    item
}

fn write_objects(objects: &[Retained<ProtocolObject<dyn NSPasteboardWriting>>]) -> Result<isize> {
    let pb = general()?;
    pb.clearContents();
    if !objects.is_empty() && !pb.writeObjects(&NSArray::from_retained_slice(objects)) {
        return Err(Error::Io("pasteboard writeObjects failed".to_string()));
    }
    Ok(pb.changeCount())
}

pub(crate) fn write(plan: &WritePlan) -> Result<isize> {
    autoreleasepool(|_| {
        let mut objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
        if !plan.data.is_empty() {
            objects.push(ProtocolObject::from_retained(item_with(&plan.data)));
        }
        for url in &plan.file_urls {
            let url = NSURL::URLWithString(&NSString::from_str(url))
                .ok_or_else(|| Error::Unsupported(format!("bad file URL '{url}'")))?;
            objects.push(ProtocolObject::from_retained(url));
        }
        write_objects(&objects)
    })
}

pub(crate) fn restore(items: &[Item]) -> Result<isize> {
    autoreleasepool(|_| {
        let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
            .iter()
            .filter(|item| !item.is_empty())
            .map(|item| ProtocolObject::from_retained(item_with(item)))
            .collect();
        write_objects(&objects)
    })
}

pub(crate) fn access() -> PasteboardAccess {
    autoreleasepool(|_| {
        let Ok(pb) = general() else {
            return PasteboardAccess::Unrestricted;
        };
        // macOS 15.4+ only.
        if !pb.respondsToSelector(sel!(accessBehavior)) {
            return PasteboardAccess::Unrestricted;
        }
        match pb.accessBehavior() {
            NSPasteboardAccessBehavior::AlwaysAllow => PasteboardAccess::Unrestricted,
            NSPasteboardAccessBehavior::AlwaysDeny => PasteboardAccess::Denied,
            _ => PasteboardAccess::Ask,
        }
    })
}

pub(crate) fn tiff_to_png(tiff: &[u8]) -> Option<Vec<u8>> {
    autoreleasepool(|_| {
        let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(tiff))?;
        // Safety: empty properties dictionary of the declared type.
        let png = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        Some(png.to_vec())
    })
}

pub(crate) fn png_to_tiff(png: &[u8]) -> Option<Vec<u8>> {
    autoreleasepool(|_| {
        NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(png))?
            .TIFFRepresentation()
            .map(|d| d.to_vec())
    })
}

fn running_app(app: &NSRunningApplication) -> RunningApp {
    RunningApp {
        pid: app.processIdentifier(),
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        name: app.localizedName().map(|s| s.to_string()),
    }
}

pub(crate) fn frontmost() -> Option<RunningApp> {
    autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| running_app(&app))
    })
}

fn live_app(pid: i32) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .filter(|app| !app.isTerminated())
}

pub(crate) fn app_for_pid(pid: i32) -> Option<RunningApp> {
    autoreleasepool(|_| live_app(pid).map(|app| running_app(&app)))
}

pub(crate) fn activate(pid: i32) -> bool {
    autoreleasepool(|_| {
        let Some(app) = live_app(pid) else {
            return false;
        };
        if app.isHidden() {
            app.unhide();
        }
        // IgnoringOtherApps is a no-op from macOS 14 but needed before it.
        #[allow(deprecated)]
        let options = NSApplicationActivationOptions::ActivateIgnoringOtherApps;
        app.activateWithOptions(options)
    })
}

pub(crate) fn accessibility_trusted(prompt: bool) -> bool {
    autoreleasepool(|_| {
        let key = sys::ax_prompt_key();
        if key.is_null() {
            return sys::ax_trusted_with(std::ptr::null());
        }
        // Safety: CFString constant bridged to NSString.
        let key: &NSString = unsafe { &*key.cast::<NSString>() };
        let value = NSNumber::new_bool(prompt);
        let options = NSDictionary::from_slices(&[key], &[&*value]);
        sys::ax_trusted_with(Retained::as_ptr(&options).cast())
    })
}
