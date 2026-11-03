//! Eager companions for private temporary text publication.
//!
//! The concrete system LCID selects the ANSI page; the system OEM page is
//! selected separately. Unavailable/custom/pseudo locales, Unicode-only pages
//! and conversion errors refuse publication before opening the clipboard.
//! Unicode remains exact. Legacy companions permit the system replacement but
//! disable best-fit mappings. UTF-8/GB18030 use their documented strict flags.
//! Raw snapshot restoration does not pass through this encoder.

use clipmonger_platform::types::{Error, Result};

pub(crate) const CF_LOCALE: u32 = 16;
pub(crate) const CF_TEXT: u32 = 1;
pub(crate) const CF_OEMTEXT: u32 = 7;
pub(crate) const CF_UNICODETEXT: u32 = 13;
const WC_ERR_INVALID_CHARS: u32 = 8;
const WC_NO_BEST_FIT_CHARS: u32 = 0x400;

struct Conversion<'a> {
    cp: u32,
    flags: u32,
    wide: &'a [u16],
    count: i32,
    default_char: Option<&'a u8>,
    used_default: Option<&'a mut i32>,
}

// Only the operating-system calls vary. Validation, counts, two-pass error
// handling and assembly below are shared by production and injected controls.
trait EncodingApi {
    fn system_lcid(&mut self) -> u32;
    fn ansi_page(&mut self, lcid: u32, page: &mut u32) -> i32;
    fn oem_page(&mut self) -> u32;
    fn convert(&mut self, request: Conversion<'_>, output: Option<&mut [u8]>) -> i32;
    fn last_error(&self) -> u32;
}

fn validate_lcid(lcid: u32) -> Result<()> {
    let language = lcid & 0xffff;
    // LCID reserved bits must be zero. A neutral LANGID, invariant locale,
    // default aliases and custom identifiers do not identify concrete text.
    // Check language bits independently of sort bits, including pseudo sorts.
    if lcid & 0xfff0_0000 != 0
        || language < 0x400
        || matches!(language, 0x0400 | 0x0800 | 0x0c00 | 0x1000 | 0x1400)
        || matches!(language, 0x0501 | 0x09ff | 0x05fe)
    {
        return Err(Error::Internal(format!(
            "system LCID {lcid:#x} is not a concrete nonpseudo locale"
        )));
    }
    Ok(())
}

fn validate_codepage(cp: u32, kind: &str) -> Result<()> {
    // 0..=3 are context-dependent page aliases, not selected concrete pages.
    if cp <= 3 || matches!(cp, 1200 | 1201 | 12000 | 12001 | 65000) {
        return Err(Error::Internal(format!(
            "system {kind} codepage {cp} is ambiguous/Unicode-only"
        )));
    }
    Ok(())
}

fn select_locale(api: &mut dyn EncodingApi) -> Result<(u32, u32, u32)> {
    let lcid = api.system_lcid();
    validate_lcid(lcid)?;
    let mut ansi = 0;
    let returned = api.ansi_page(lcid, &mut ansi);
    if returned != 2 {
        return Err(Error::Internal(format!(
            "GetLocaleInfoW ANSI page failed: return {returned}, error {}",
            api.last_error()
        )));
    }
    validate_codepage(ansi, "ANSI")?;
    let oem = api.oem_page();
    validate_codepage(oem, "OEM")?;
    Ok((lcid, ansi, oem))
}

fn checked_wide_count(count: usize) -> Result<i32> {
    i32::try_from(count)
        .map_err(|_| Error::Internal("text too large for WideCharToMultiByte".into()))
}

fn encode_page(api: &mut dyn EncodingApi, wide: &[u16], cp: u32) -> Result<Vec<u8>> {
    let count = checked_wide_count(wide.len())?;
    // Both default-character pointers are NULL, including for UTF-8 where
    // that is mandatory. Ordinary pages use the system replacement and
    // WC_NO_BEST_FIT_CHARS. Pages that cannot accept this policy fail the API
    // call; there is no retry with weaker flags or raw UTF-8 fallback.
    let flags = if matches!(cp, 65001 | 54936) {
        WC_ERR_INVALID_CHARS
    } else {
        WC_NO_BEST_FIT_CHARS
    };
    let request = || Conversion {
        cp,
        flags,
        wide,
        count,
        default_char: None,
        used_default: None,
    };
    let needed = api.convert(request(), None);
    if needed <= 0 {
        return Err(Error::Internal(format!(
            "WideCharToMultiByte sizing failed cp {cp}, error {}",
            api.last_error()
        )));
    }
    let mut out = vec![0; usize::try_from(needed).expect("positive i32 fits usize")];
    let written = api.convert(request(), Some(&mut out));
    if written != needed || out.last() != Some(&0) {
        return Err(Error::Internal(format!(
            "WideCharToMultiByte output length/NUL mismatch cp {cp}: expected {needed}, got {written}, error {}",
            api.last_error()
        )));
    }
    Ok(out)
}

fn encode_with_api(
    api: &mut dyn EncodingApi,
    text: &str,
    lcid: u32,
    ansi: u32,
    oem: u32,
) -> Result<Vec<(u32, Vec<u8>)>> {
    if text.contains('\0') {
        return Err(Error::Internal("text payload contains embedded NUL".into()));
    }
    validate_lcid(lcid)?;
    validate_codepage(ansi, "ANSI")?;
    validate_codepage(oem, "OEM")?;
    let wide_len = text
        .encode_utf16()
        .count()
        .checked_add(1)
        .ok_or_else(|| Error::Internal("text UTF-16 length overflow".into()))?;
    checked_wide_count(wide_len)?;
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let unicode = wide.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let ansi = encode_page(api, &wide, ansi)?;
    let oem = encode_page(api, &wide, oem)?;
    Ok(vec![
        (CF_UNICODETEXT, unicode),
        (CF_LOCALE, lcid.to_le_bytes().to_vec()),
        (CF_TEXT, ansi),
        (CF_OEMTEXT, oem),
    ])
}

pub(crate) fn text_locale_and_pages() -> Result<(u32, u32, u32)> {
    with_api(select_locale)
}

pub(crate) fn encode_text_companions_with_locale(
    text: &str,
    lcid: u32,
    ansi: u32,
    oem: u32,
) -> Result<Vec<(u32, Vec<u8>)>> {
    with_api(|api| encode_with_api(api, text, lcid, ansi, oem))
}

fn with_api<T>(action: impl FnOnce(&mut dyn EncodingApi) -> Result<T>) -> Result<T> {
    #[cfg(test)]
    {
        let mut action = Some(action);
        let injected = testing::INJECTED.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map(|api| action.take().unwrap()(api))
        });
        if let Some(result) = injected {
            return result;
        }
        native_api(action.unwrap())
    }
    #[cfg(not(test))]
    native_api(action)
}

#[cfg(windows)]
fn native_api<T>(action: impl FnOnce(&mut dyn EncodingApi) -> Result<T>) -> Result<T> {
    action(&mut NativeApi)
}

#[cfg(not(windows))]
fn native_api<T>(_action: impl FnOnce(&mut dyn EncodingApi) -> Result<T>) -> Result<T> {
    Err(Error::Unavailable(
        "Windows text encoding needs an explicitly injected test API".into(),
    ))
}

#[cfg(windows)]
struct NativeApi;
#[cfg(windows)]
#[allow(unsafe_code)] // Direct Win32 text conversion FFI boundary.
impl EncodingApi for NativeApi {
    fn system_lcid(&mut self) -> u32 {
        // SAFETY: reads the system locale; takes no pointers.
        unsafe { windows_sys::Win32::Globalization::GetSystemDefaultLCID() }
    }
    fn ansi_page(&mut self, lcid: u32, page: &mut u32) -> i32 {
        use windows_sys::Win32::Globalization::{
            GetLocaleInfoW, LOCALE_IDEFAULTANSICODEPAGE, LOCALE_RETURN_NUMBER,
        };
        // SAFETY: the numeric DWORD buffer occupies exactly two WCHARs.
        unsafe {
            GetLocaleInfoW(
                lcid,
                LOCALE_IDEFAULTANSICODEPAGE | LOCALE_RETURN_NUMBER,
                std::ptr::from_mut(page).cast::<u16>(),
                2,
            )
        }
    }
    fn oem_page(&mut self) -> u32 {
        // SAFETY: no arguments or pointers.
        unsafe { windows_sys::Win32::Globalization::GetOEMCP() }
    }
    fn convert(&mut self, request: Conversion<'_>, output: Option<&mut [u8]>) -> i32 {
        let (pointer, length) = output.map_or((std::ptr::null_mut(), 0), |out| {
            (
                out.as_mut_ptr(),
                i32::try_from(out.len()).expect("sized by positive i32"),
            )
        });
        // SAFETY: input count was checked and includes its NUL; output is
        // either NULL/0 for sizing or a distinct allocation of the sized length.
        // References (if supplied) remain live through the call.
        unsafe {
            windows_sys::Win32::Globalization::WideCharToMultiByte(
                request.cp,
                request.flags,
                request.wide.as_ptr(),
                request.count,
                pointer,
                length,
                request
                    .default_char
                    .map_or(std::ptr::null(), std::ptr::from_ref),
                request
                    .used_default
                    .map_or(std::ptr::null_mut(), std::ptr::from_mut),
            )
        }
    }
    fn last_error(&self) -> u32 {
        // SAFETY: no pointers; only called immediately after a failed API call.
        unsafe { windows_sys::Win32::Foundation::GetLastError() }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::{Conversion, EncodingApi};
    use std::cell::RefCell;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Call {
        Lcid,
        Ansi(u32),
        Oem,
        Convert {
            cp: u32,
            flags: u32,
            wide: Vec<u16>,
            count: i32,
            output: usize,
            default_char_null: bool,
            used_default_null: bool,
        },
    }
    #[derive(Clone, Copy)]
    pub(crate) enum Fault {
        LocaleReturn(i32),
        Size(u32, i32),
        Write(u32, i32),
        MissingNul(u32),
    }
    pub(super) struct TestApi {
        locale: (u32, u32, u32),
        fault: Option<Fault>,
        calls: Vec<Call>,
    }
    thread_local! {
        pub(super) static INJECTED: RefCell<Option<TestApi>> = const { RefCell::new(None) };
    }
    pub(crate) fn set_injected_locale(lcid: u32, ansi: u32, oem: u32) {
        INJECTED.with(|cell| {
            *cell.borrow_mut() = Some(TestApi {
                locale: (lcid, ansi, oem),
                fault: None,
                calls: vec![],
            });
        });
    }
    pub(crate) fn ensure_injected_api() {
        if INJECTED.with(|cell| cell.borrow().is_none()) {
            set_injected_locale(0x0409, 1252, 437);
        }
    }
    pub(crate) fn clear_injected_locale() {
        INJECTED.with(|cell| *cell.borrow_mut() = None);
    }
    pub(crate) fn inject_fault(fault: Fault) {
        ensure_injected_api();
        INJECTED.with(|cell| cell.borrow_mut().as_mut().unwrap().fault = Some(fault));
    }
    pub(crate) fn calls() -> Vec<Call> {
        INJECTED.with(|cell| cell.borrow().as_ref().unwrap().calls.clone())
    }
    impl EncodingApi for TestApi {
        fn system_lcid(&mut self) -> u32 {
            self.calls.push(Call::Lcid);
            self.locale.0
        }
        fn ansi_page(&mut self, lcid: u32, page: &mut u32) -> i32 {
            self.calls.push(Call::Ansi(lcid));
            *page = self.locale.1;
            if let Some(Fault::LocaleReturn(result)) = self.fault {
                result
            } else {
                2
            }
        }
        fn oem_page(&mut self) -> u32 {
            self.calls.push(Call::Oem);
            self.locale.2
        }
        fn convert(&mut self, request: Conversion<'_>, output: Option<&mut [u8]>) -> i32 {
            assert!(request.default_char.is_none());
            assert!(request.used_default.is_none());
            self.calls.push(Call::Convert {
                cp: request.cp,
                flags: request.flags,
                wide: request.wide.to_vec(),
                count: request.count,
                output: output.as_ref().map_or(0, |out| out.len()),
                default_char_null: request.default_char.is_none(),
                used_default_null: request.used_default.is_none(),
            });
            if let Some(Fault::Size(cp, result)) = self.fault {
                if cp == request.cp && output.is_none() {
                    return result;
                }
            }
            // Software fixtures supply only API conversion output. This is
            // deliberately not evidence of actual Windows conversion behavior.
            let text = String::from_utf16(request.wide).unwrap();
            let mut bytes = Vec::new();
            for ch in text.chars() {
                match (request.cp, ch) {
                    (65001 | 54936, _) => {
                        let mut buffer = [0; 4];
                        bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                    }
                    (_, ch) if ch.is_ascii() => bytes.push(u8::try_from(u32::from(ch)).unwrap()),
                    (1252, 'é') => bytes.push(0xe9),
                    (437, 'é') => bytes.push(0x82),
                    (932, 'あ') => bytes.extend_from_slice(&[0x82, 0xa0]),
                    // Only these fixture legacy pages model one default per
                    // unmapped UTF-16 unit, including both surrogate units.
                    (1252 | 437 | 932, ch) => bytes.resize(bytes.len() + ch.len_utf16(), b'?'),
                    _ => bytes.push(b'?'),
                }
            }
            let length = i32::try_from(bytes.len()).unwrap();
            if let Some(out) = output {
                assert_eq!(out.len(), bytes.len());
                out.copy_from_slice(&bytes);
                if let Some(Fault::MissingNul(cp)) = self.fault {
                    if cp == request.cp {
                        *out.last_mut().unwrap() = 1;
                    }
                }
                if let Some(Fault::Write(cp, result)) = self.fault {
                    if cp == request.cp {
                        return result;
                    }
                }
            }
            length
        }
        fn last_error(&self) -> u32 {
            87
        }
    }
}

#[cfg(test)]
pub(crate) use testing::{clear_injected_locale, set_injected_locale};

#[cfg(test)]
mod tests {
    #[test]
    fn wide_count_checks_i32_boundary_without_allocation() {
        assert_eq!(
            super::checked_wide_count(i32::MAX as usize).unwrap(),
            i32::MAX
        );
        assert!(super::checked_wide_count(i32::MAX as usize + 1).is_err());
    }
}

#[cfg(all(test, windows))]
mod native_tests {
    // Compile on the Windows target now; execution needs the separately
    // authorized native environment. This bypasses all injected adapters.
    #[test]
    fn actual_converter_independent_legacy_bytes() {
        let mut api = super::NativeApi;
        let ops = super::encode_with_api(&mut api, "café", 0x040c, 1252, 437).unwrap();
        assert_eq!(
            ops,
            vec![
                (13, vec![99, 0, 97, 0, 102, 0, 0xe9, 0, 0, 0]),
                (16, vec![12, 4, 0, 0]),
                (1, vec![99, 97, 102, 0xe9, 0]),
                (7, vec![99, 97, 102, 0x82, 0]),
            ]
        );
        let ops = super::encode_with_api(&mut api, "あ", 0x0411, 932, 437).unwrap();
        assert_eq!(ops[2].1, vec![0x82, 0xa0, 0]);
        // Fixed legacy-page expectations: supplementary loss uses two defaults;
        // BMP loss uses one. Literal question marks must survive unchanged.
        for (text, unicode, legacy) in [
            (
                "a🌍b",
                vec![97, 0, 60, 216, 13, 223, 98, 0, 0, 0],
                b"a??b\0".to_vec(),
            ),
            ("a☃b", vec![97, 0, 3, 38, 98, 0, 0, 0], b"a?b\0".to_vec()),
            (
                "a??b",
                vec![97, 0, 63, 0, 63, 0, 98, 0, 0, 0],
                b"a??b\0".to_vec(),
            ),
        ] {
            let ops = super::encode_with_api(&mut api, text, 0x0409, 1252, 437).unwrap();
            // Emit both actual outputs before any assertion can hide the OEM result.
            eprintln!("{text:?}: CP1252={:?}; CP437={:?}", ops[2].1, ops[3].1);
            assert_eq!(
                ops,
                vec![
                    (13, unicode),
                    (16, vec![9, 4, 0, 0]),
                    (1, legacy.clone()),
                    (7, legacy),
                ],
                "fixed legacy fixture {text:?}"
            );
        }
    }
}
