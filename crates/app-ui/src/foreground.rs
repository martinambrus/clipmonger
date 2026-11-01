//! The Windows foreground right the UI passes on.

/// Let any process take the foreground once (until the next input): the UI
/// calls it while it is the foreground window, before it hides for a paste or
/// hands over to the running UI on a second launch, so that the daemon or the
/// helper can hand the focus back to the paste target. Windows only
/// (`AllowSetForegroundWindow(ASFW_ANY)`); does nothing elsewhere.
pub fn allow_any() {
    #[cfg(windows)]
    {
        // Safety: no pointers; without the right the call just fails.
        #[allow(unsafe_code)]
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
            );
        }
    }
}
