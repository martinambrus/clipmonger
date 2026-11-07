//! Main-thread event loop. `AppKit` (global hotkeys via Carbon, activation,
//! workspace notifications) needs the main thread's run loop: the daemon
//! hands its work to [`run`], which keeps the main thread in
//! `NSApplication.run` as an accessory app (no Dock icon) and exits the
//! process with the work's code. Off macOS, [`run`] just runs the work.

use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Whether [`run`] is serving the main thread.
#[must_use]
pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

/// Run `work` on a new thread while the main thread runs the `AppKit` loop;
/// the process exits with `work`'s code (101 if it panics).
///
/// # Panics
///
/// When called off the main thread.
pub fn run(work: impl FnOnce() -> i32 + Send + 'static) -> ! {
    let guarded =
        move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or(101);
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

        let mtm =
            MainThreadMarker::new().expect("main_loop::run must be called on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        RUNNING.store(true, Ordering::Release);
        std::thread::Builder::new()
            .name("clipmonger-main-work".to_string())
            .spawn(move || std::process::exit(guarded()))
            .expect("spawn work thread");
        app.run();
        std::process::exit(0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        RUNNING.store(true, Ordering::Release);
        std::process::exit(guarded())
    }
}

/// Run `f` synchronously on the main thread (directly when already there,
/// else `dispatch_sync` to the main queue). Blocks until the main thread
/// serves its queue, so off the main thread it needs [`run`] first.
pub fn on_main<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    #[cfg(target_os = "macos")]
    {
        dispatch2::run_on_main(|_| f())
    }
    #[cfg(not(target_os = "macos"))]
    {
        f()
    }
}

/// Whether the current thread is the process main thread.
#[must_use]
pub fn is_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2::MainThreadMarker::new().is_some()
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::thread::current().name() == Some("main")
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn on_main_runs_inline_off_macos() {
        let value = std::thread::spawn(|| on_main(|| 7)).join().unwrap();
        assert_eq!(value, 7);
        assert!(!is_running());
    }
}
