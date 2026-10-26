//! `wlprobe`: registry globals dump + Watcher construction attempt.
//!
//! Machine-readable lines on stdout:
//! `global <interface> <version>` per advertised global, then exactly one
//! `watcher: ok|missing-protocol|error <detail>` line. Exit 0 always on a
//! successful registry read (the watcher outcome is evidence, not an
//! assertion); non-zero only when the registry itself cannot be read.

#[cfg(unix)]
fn main() {
    std::process::exit(real::run());
}

#[cfg(unix)]
mod real {
    use wayland_client::protocol::wl_registry::{Event as RegistryEvent, WlRegistry};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wl_clipboard_rs::paste::{Error as PasteError, Seat};
    use wl_clipboard_rs::watch::ClipboardType;

    struct State {
        globals: Vec<(u32, String, u32)>,
    }

    impl Dispatch<WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            _: &WlRegistry,
            event: <WlRegistry as Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let RegistryEvent::Global {
                name,
                interface,
                version,
            } = event
            {
                state.globals.push((name, interface, version));
            }
        }
    }

    pub fn run() -> i32 {
        let Ok(conn) = Connection::connect_to_env() else {
            eprintln!("wlprobe: cannot connect to Wayland compositor");
            return 1;
        };
        let display = conn.display();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let _registry = display.get_registry(&qh, ());
        let mut state = State {
            globals: Vec::new(),
        };
        if queue.roundtrip(&mut state).is_err() {
            eprintln!("wlprobe: registry roundtrip failed");
            return 1;
        }
        state.globals.sort_by(|a, b| a.1.cmp(&b.1));
        for (_, interface, version) in &state.globals {
            println!("global {interface} {version}");
        }
        let has_data_control = state.globals.iter().any(|(_, interface, _)| {
            interface == "ext_data_control_manager_v1"
                || interface == "zwlr_data_control_manager_v1"
        });
        match wl_clipboard_rs::watch::Watcher::new(ClipboardType::Regular, Seat::Unspecified) {
            Ok(_) => println!("watcher: ok"),
            Err(PasteError::MissingProtocol { name, version }) => {
                println!("watcher: missing-protocol {name} v{version}");
            }
            Err(err) if !has_data_control => {
                println!("watcher: missing-protocol {err}");
            }
            Err(err) => println!("watcher: error {err}"),
        }
        0
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("wlprobe: unix Wayland only");
    std::process::exit(2);
}
