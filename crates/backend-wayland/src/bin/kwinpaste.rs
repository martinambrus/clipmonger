//! `kwinpaste [--shift] [delay_ms]`: press the paste chord through `KWin`'s
//! fake-input protocol after an optional delay. A manual check tool.

#[cfg(unix)]
fn main() {
    let mut shift = false;
    let mut delay = 0u64;
    for arg in std::env::args().skip(1) {
        if arg == "--shift" {
            shift = true;
        } else if let Ok(ms) = arg.parse() {
            delay = ms;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(delay));
    let result = clipmonger_backend_wayland::fake_input::FakeInput::connect()
        .and_then(|mut input| input.paste_chord(shift));
    match result {
        Ok(()) => println!("chord sent"),
        Err(e) => {
            eprintln!("kwinpaste: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(unix))]
fn main() {}
