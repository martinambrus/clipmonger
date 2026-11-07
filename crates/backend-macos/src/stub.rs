//! Off-macOS stand-in for `appkit`: same signatures, every call reports
//! `Unavailable` so the neutral modules compile and test everywhere.

use clipmonger_platform::types::{Error, Result};

use crate::caps::PasteboardAccess;
use crate::foreground::RunningApp;
use crate::mime::{Item, TypeNames, WritePlan};

fn unavailable<T>() -> Result<T> {
    Err(Error::Unavailable("macOS backend needs macOS".to_string()))
}

pub(crate) fn type_names() -> Result<TypeNames> {
    unavailable()
}
pub(crate) fn change_count() -> Result<isize> {
    unavailable()
}
pub(crate) fn types() -> Result<Vec<String>> {
    unavailable()
}
pub(crate) fn data(_uti: &str, _max: u64) -> Result<Option<Vec<u8>>> {
    unavailable()
}
pub(crate) fn string_utf8(_max: u64) -> Result<Option<Vec<u8>>> {
    unavailable()
}
pub(crate) fn file_urls(_file_url_type: &str) -> Result<Vec<String>> {
    unavailable()
}
pub(crate) fn snapshot(_max_total: u64) -> Result<Vec<Item>> {
    unavailable()
}
pub(crate) fn write(_plan: &WritePlan) -> Result<isize> {
    unavailable()
}
pub(crate) fn restore(_items: &[Item]) -> Result<isize> {
    unavailable()
}
pub(crate) fn access() -> PasteboardAccess {
    PasteboardAccess::Unrestricted
}
pub(crate) fn tiff_to_png(_tiff: &[u8]) -> Option<Vec<u8>> {
    None
}
pub(crate) fn png_to_tiff(_png: &[u8]) -> Option<Vec<u8>> {
    None
}
pub(crate) fn frontmost() -> Option<RunningApp> {
    None
}
pub(crate) fn app_for_pid(_pid: i32) -> Option<RunningApp> {
    None
}
pub(crate) fn activate(_pid: i32) -> bool {
    false
}
pub(crate) fn accessibility_trusted(_prompt: bool) -> bool {
    false
}
pub(crate) fn secure_input_enabled() -> bool {
    false
}
pub(crate) fn post_key(_code: u16, _down: bool, _flags: u64) -> Result<()> {
    unavailable()
}
pub(crate) fn post_unicode(_units: &[u16]) -> Result<()> {
    unavailable()
}
