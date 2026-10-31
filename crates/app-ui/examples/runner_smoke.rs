#![cfg_attr(windows, windows_subsystem = "windows")]

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipmonger_app_ui::runner::{run, App, Config, Desired, WindowKind};
use serde_json::json;

fn write_trace(
    trace: &Arc<Mutex<File>>,
    start: Instant,
    mut map: serde_json::Map<String, serde_json::Value>,
) {
    let t_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let unix_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    map.insert("t_ms".to_string(), json!(t_ms));
    map.insert("unix_ms".to_string(), json!(unix_ms));
    let value = serde_json::Value::Object(map);
    let mut line = serde_json::to_string(&value).unwrap();
    line.push('\n');
    if let Ok(mut file) = trace.lock() {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

struct SmokeApp {
    start: Instant,
    trace: Arc<Mutex<File>>,
    control_path: Option<PathBuf>,
    last_seq: u64,
    popup_open: bool,
    settings_open: bool,
    exit: bool,
    exit_at: Option<Instant>,
    logic_popup: usize,
    logic_settings: usize,
    ui_popup: usize,
    ui_settings: usize,
    wake_queue: Arc<Mutex<Vec<(u64, Instant)>>>,
}

impl SmokeApp {
    fn poll_control(&mut self) {
        let Some(path) = self.control_path.clone() else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let mut entries: Vec<(u64, String)> = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, ' ');
            let seq_str = parts.next().unwrap_or("");
            let cmd = parts.next().unwrap_or("").to_string();
            if let Ok(seq) = seq_str.parse::<u64>() {
                entries.push((seq, cmd));
            }
        }
        for (seq, cmd) in entries {
            if seq > self.last_seq {
                match cmd.as_str() {
                    "open" => self.popup_open = true,
                    "close" => self.popup_open = false,
                    "settings" => self.settings_open = true,
                    "settings-close" => self.settings_open = false,
                    "quit" => self.exit = true,
                    _ => {}
                }
                self.last_seq = seq;
                let mut map = serde_json::Map::new();
                map.insert("ev".to_string(), json!("cmd"));
                map.insert("seq".to_string(), json!(seq));
                map.insert("cmd".to_string(), json!(cmd));
                write_trace(&self.trace, self.start, map);
            }
        }
    }
}

impl App for SmokeApp {
    fn logic(&mut self, ctx: &egui::Context, kind: WindowKind) {
        let n = match kind {
            WindowKind::Popup => {
                self.logic_popup += 1;
                self.logic_popup
            }
            WindowKind::Settings => {
                self.logic_settings += 1;
                self.logic_settings
            }
        };
        let mut map = serde_json::Map::new();
        map.insert("ev".to_string(), json!("logic"));
        map.insert(
            "kind".to_string(),
            json!(match kind {
                WindowKind::Popup => "popup",
                WindowKind::Settings => "settings",
            }),
        );
        map.insert("n".to_string(), json!(n));
        write_trace(&self.trace, self.start, map);

        if ctx.input(|i| i.viewport().close_requested()) {
            match kind {
                WindowKind::Popup => self.popup_open = false,
                WindowKind::Settings => self.settings_open = false,
            }
        }

        if let Some(deadline) = self.exit_at {
            if Instant::now() >= deadline {
                self.exit = true;
            }
        }

        if kind == WindowKind::Popup {
            let pending = {
                let mut q = self.wake_queue.lock().unwrap();
                std::mem::take(&mut *q)
            };
            for (id, sent_at) in pending {
                let latency_ms = Instant::now().duration_since(sent_at).as_secs_f64() * 1000.0;
                let mut map = serde_json::Map::new();
                map.insert("ev".to_string(), json!("wake_seen"));
                map.insert("id".to_string(), json!(id));
                map.insert("latency_ms".to_string(), json!(latency_ms));
                write_trace(&self.trace, self.start, map);
            }
            self.poll_control();
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, kind: WindowKind) {
        let n = match kind {
            WindowKind::Popup => {
                self.ui_popup += 1;
                self.ui_popup
            }
            WindowKind::Settings => {
                self.ui_settings += 1;
                self.ui_settings
            }
        };
        let mut map = serde_json::Map::new();
        map.insert("ev".to_string(), json!("ui"));
        map.insert(
            "kind".to_string(),
            json!(match kind {
                WindowKind::Popup => "popup",
                WindowKind::Settings => "settings",
            }),
        );
        map.insert("n".to_string(), json!(n));
        write_trace(&self.trace, self.start, map);

        match kind {
            WindowKind::Popup => {
                ui.heading("clipmonger smoke");
                ui.label(format!("ui calls: {n}"));
                if ui.button("close").clicked() {
                    self.popup_open = false;
                }
            }
            WindowKind::Settings => {
                ui.heading("clipmonger smoke settings");
                ui.label(format!("ui calls: {n}"));
            }
        }
    }

    fn desired(&self) -> Desired {
        Desired {
            popup_open: self.popup_open,
            settings_open: self.settings_open,
            exit: self.exit,
        }
    }
}

struct Args {
    trace: PathBuf,
    control: Option<PathBuf>,
    seconds: Option<u64>,
    open: bool,
    wake_every_ms: Option<u64>,
    position: Option<[f32; 2]>,
}

fn parse_args() -> Option<Args> {
    let args: Vec<String> = std::env::args().collect();
    let mut trace: Option<PathBuf> = None;
    let mut control: Option<PathBuf> = None;
    let mut seconds: Option<u64> = None;
    let mut open = false;
    let mut wake_every_ms: Option<u64> = None;
    let mut position: Option<[f32; 2]> = None;
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--trace" {
            i += 1;
            if i < args.len() {
                trace = Some(PathBuf::from(&args[i]));
            }
        } else if let Some(v) = arg.strip_prefix("--trace=") {
            trace = Some(PathBuf::from(v));
        } else if arg == "--control" {
            i += 1;
            if i < args.len() {
                control = Some(PathBuf::from(&args[i]));
            }
        } else if let Some(v) = arg.strip_prefix("--control=") {
            control = Some(PathBuf::from(v));
        } else if arg == "--seconds" {
            i += 1;
            if i < args.len() {
                if let Ok(n) = args[i].parse::<u64>() {
                    seconds = Some(n);
                }
            }
        } else if let Some(v) = arg.strip_prefix("--seconds=") {
            if let Ok(n) = v.parse::<u64>() {
                seconds = Some(n);
            }
        } else if arg == "--open" {
            open = true;
        } else if arg == "--wake-every-ms" {
            i += 1;
            if i < args.len() {
                if let Ok(n) = args[i].parse::<u64>() {
                    wake_every_ms = Some(n);
                }
            }
        } else if let Some(v) = arg.strip_prefix("--wake-every-ms=") {
            if let Ok(n) = v.parse::<u64>() {
                wake_every_ms = Some(n);
            }
        } else if arg == "--position" {
            i += 1;
            if i < args.len() {
                let v = &args[i];
                if let Some((xs, ys)) = v.split_once(',') {
                    if let (Ok(x), Ok(y)) = (xs.parse::<f32>(), ys.parse::<f32>()) {
                        position = Some([x, y]);
                    }
                }
            }
        } else if let Some(v) = arg.strip_prefix("--position=") {
            if let Some((xs, ys)) = v.split_once(',') {
                if let (Ok(x), Ok(y)) = (xs.parse::<f32>(), ys.parse::<f32>()) {
                    position = Some([x, y]);
                }
            }
        }
        i += 1;
    }
    let trace = trace?;
    Some(Args {
        trace,
        control,
        seconds,
        open,
        wake_every_ms,
        position,
    })
}

fn main() {
    let start = Instant::now();
    let Some(Args {
        trace: trace_path,
        control: control_path,
        seconds,
        open,
        wake_every_ms,
        position,
    }) = parse_args()
    else {
        std::process::exit(1);
    };

    let Ok(file) = File::create(&trace_path) else {
        std::process::exit(1);
    };
    let trace = Arc::new(Mutex::new(file));
    let wake_queue: Arc<Mutex<Vec<(u64, Instant)>>> = Arc::new(Mutex::new(Vec::new()));

    {
        let mut map = serde_json::Map::new();
        map.insert("ev".to_string(), json!("start"));
        map.insert("pid".to_string(), json!(std::process::id()));
        write_trace(&trace, start, map);
    }

    let config = Config {
        popup_title: "clipmonger smoke".to_string(),
        popup_size: [480.0, 360.0],
        popup_position: position,
        settings_title: "clipmonger smoke settings".to_string(),
        settings_size: [400.0, 300.0],
    };

    let trace_clone = trace.clone();
    let wake_queue_clone = wake_queue.clone();
    let control_clone = control_path.clone();
    let exit_at = seconds.map(|s| start + Duration::from_secs(s));

    let result = run(config, move |startup| {
        {
            let mut map = serde_json::Map::new();
            map.insert("ev".to_string(), json!("ready"));
            map.insert("hwnd".to_string(), json!(startup.popup_hwnd));
            write_trace(&trace_clone, start, map);
        }

        if let Some(interval) = wake_every_ms {
            let handle = startup.handle.clone();
            let trace_thr = trace_clone.clone();
            let queue_thr = wake_queue_clone.clone();
            let start_thr = start;
            std::thread::spawn(move || {
                let mut id: u64 = 1;
                loop {
                    std::thread::sleep(Duration::from_millis(interval));
                    let sent_at = Instant::now();
                    let mut map = serde_json::Map::new();
                    map.insert("ev".to_string(), json!("wake_sent"));
                    map.insert("id".to_string(), json!(id));
                    write_trace(&trace_thr, start_thr, map);
                    {
                        let mut q = queue_thr.lock().unwrap();
                        q.push((id, sent_at));
                    }
                    handle.wake();
                    id += 1;
                }
            });
        }

        SmokeApp {
            start,
            trace: trace_clone,
            control_path: control_clone,
            last_seq: 0,
            popup_open: open,
            settings_open: false,
            exit: false,
            exit_at,
            logic_popup: 0,
            logic_settings: 0,
            ui_popup: 0,
            ui_settings: 0,
            wake_queue: wake_queue_clone,
        }
    });

    let ok = result.is_ok();
    let mut map = serde_json::Map::new();
    map.insert("ev".to_string(), json!("exit"));
    map.insert("ok".to_string(), json!(ok));
    write_trace(&trace, start, map);
}
