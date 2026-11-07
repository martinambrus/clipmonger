//! Print the capture events `MacMonitor` reports for 8 s.
use clipmonger_platform::traits::ClipboardMonitor as _;
use clipmonger_platform::types::SubscribeKind;

fn main() {
    let mut monitor =
        clipmonger_backend_macos::MacMonitor::new(clipmonger_backend_macos::MonitorOpts::default())
            .expect("monitor");
    monitor
        .subscribe(&[SubscribeKind::Clipboard])
        .expect("subscribe");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while std::time::Instant::now() < end {
        for event in monitor
            .poll(std::time::Duration::from_millis(500))
            .expect("poll")
        {
            println!("event: {event:?}");
        }
    }
}
