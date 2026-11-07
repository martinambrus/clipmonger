//! Popup-owned thumbnail demand, CPU cache and context-local textures.
use crate::thumbnail_worker::{Admission, Cache, CacheState, Key, Worker};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub(super) struct Thumbnails {
    worker: Worker,
    cache: Cache,
    context: Option<egui::Context>,
    generation: u64,
    textures: HashMap<Key, egui::TextureHandle>,
    #[cfg(test)]
    pub(super) slots: Vec<(uuid::Uuid, egui::Rect)>,
    #[cfg(test)]
    pub(super) history_area: Option<(egui::Rect, egui::Vec2)>,
    #[cfg(test)]
    deferred: bool,
}
impl Thumbnails {
    pub fn new(worker: Worker) -> Self {
        Self {
            worker,
            cache: Cache::default(),
            context: None,
            generation: 1,
            textures: HashMap::new(),
            #[cfg(test)]
            slots: Vec::new(),
            #[cfg(test)]
            history_area: None,
            #[cfg(test)]
            deferred: false,
        }
    }
    #[allow(clippy::unused_self)] // Test builds collect per-frame viewport data.
    pub fn begin_frame(&mut self) {
        #[cfg(test)]
        {
            self.slots.clear();
            self.history_area = None;
        }
    }
    pub fn context(&mut self, ctx: &egui::Context) {
        if self.context.as_ref() != Some(ctx) {
            self.reset();
            self.context = Some(ctx.clone());
        }
    }
    pub fn reset(&mut self) {
        self.worker.set_visible([]);
        self.cache = Cache::default();
        self.textures.clear();
        self.generation = self.generation.saturating_add(1);
    }
    pub fn revoke_visibility(&mut self) {
        self.worker.set_visible([]);
        self.cache.set_visible([]);
        self.textures.clear();
    }
    pub fn drain(&mut self) {
        while let Some(result) = self.worker.try_recv() {
            self.cache.accept(result, Instant::now());
        }
    }
    fn key(&self, id: uuid::Uuid, hash: Option<&str>) -> Option<Key> {
        let hash = hash.filter(|hash| clipmonger_core::model::valid_clip_content_hash(hash))?;
        Some(Key {
            clip: id,
            content_hash: hash.to_string(),
            max_px: 128,
            generation: self.generation,
            context: self.generation,
        })
    }
    pub fn slot(
        &mut self,
        ui: &mut egui::Ui,
        id: uuid::Uuid,
        hash: Option<&str>,
        demand: &mut Vec<Key>,
    ) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
        let key = self.key(id, hash);
        if !ui.is_rect_visible(rect) {
            return;
        }
        #[cfg(test)]
        self.slots.push((id, rect));
        if let Some(key) = &key {
            demand.push(key.clone());
        }
        let Some(key) = key else {
            placeholder(ui, rect);
            return;
        };
        let Some(pixels) = self.cache.pixels(&key) else {
            self.textures.remove(&key);
            placeholder(ui, rect);
            return;
        };
        if !self.textures.contains_key(&key) {
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width as usize, pixels.height as usize],
                &pixels.rgba,
            );
            let texture = ui
                .ctx()
                .load_texture("thumbnail", color, egui::TextureOptions::LINEAR);
            self.textures.insert(key.clone(), texture);
        }
        if let Some(texture) = self.textures.get(&key) {
            let source = texture.size_vec2();
            let scale = (rect.width() / source.x).min(rect.height() / source.y);
            let target = egui::Rect::from_center_size(rect.center(), source * scale);
            ui.painter().image(
                texture.id(),
                target,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            placeholder(ui, rect);
        }
    }
    pub fn update(&mut self, ctx: &egui::Context, demand: Vec<Key>) {
        let mut unique = HashSet::new();
        let keys: Vec<Key> = demand
            .into_iter()
            .filter(|key| unique.insert(key.clone()))
            .take(64)
            .collect();
        self.worker.set_visible(keys.clone());
        self.cache.set_visible(keys.clone());
        self.textures
            .retain(|key, _| unique.contains(key) && self.cache.pixels(key).is_some());
        let now = Instant::now();
        let mut retry_deadline = None;
        for key in keys {
            let state = self.cache.state(&key, now);
            if state == CacheState::Ready
                && !self.textures.contains_key(&key)
                && self.cache.pixels(&key).is_some()
            {
                ctx.request_repaint();
            }
            if state == CacheState::Cooldown {
                if let Some(deadline) = self.cache.retry_deadline(&key, now) {
                    retry_deadline = Some(
                        retry_deadline.map_or(deadline, |earliest: Instant| earliest.min(deadline)),
                    );
                }
                continue;
            }
            if let Some(request) = self.cache.begin(&key, now) {
                match self.worker.submit(request.clone()) {
                    Admission::Queued | Admission::Duplicate => (),
                    Admission::Deferred => {
                        #[cfg(test)]
                        {
                            self.deferred = true;
                        }
                        self.cache.abandon(&request);
                    }
                    Admission::Obsolete | Admission::Closed => self.cache.abandon(&request),
                }
            }
        }
        if let Some(deadline) = retry_deadline {
            ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        }
    }
}
pub(super) fn placeholder(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter()
        .rect_filled(rect.shrink(2.0), 3.0, ui.visuals().widgets.inactive.bg_fill);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::{AppParts, PopupApp};
    use crate::runner::{App, WindowKind};
    use clipmonger_core::model::Clip;
    use clipmonger_ipc::{
        framing::{decode_line, encode_line},
        proto::{Envelope, FrameKind, IPC_VERSION},
    };
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    struct Profile(std::path::PathBuf);
    impl Profile {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cm-visible-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("token"), "token").unwrap();
            Self(path)
        }
    }
    impl Drop for Profile {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    struct Peer(Option<std::thread::JoinHandle<()>>);
    impl Peer {
        fn join(mut self) {
            self.0.take().unwrap().join().unwrap();
        }
    }
    impl Drop for Peer {
        fn drop(&mut self) {
            if let Some(thread) = self.0.take() {
                let result = thread.join();
                if !std::thread::panicking() {
                    result.unwrap();
                }
            }
        }
    }
    struct Release(Option<mpsc::SyncSender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.try_send(());
            }
        }
    }
    #[allow(clippy::needless_pass_by_value)] // Fixtures construct owned format lists.
    fn clip(id: u128, formats: Vec<&str>, hash: &str) -> Clip {
        serde_json::from_value(serde_json::json!({"id":uuid::Uuid::from_u128(id),"created_ms":1,"source_os":"test","preview":format!("row {id}"),"formats":formats,"content_hash":hash,"rev":{"wall_ms":1,"dev":"test"}})).unwrap()
    }
    fn base64(bytes: &[u8]) -> String {
        let abc = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let a = c[0];
            let b = *c.get(1).unwrap_or(&0);
            let d = *c.get(2).unwrap_or(&0);
            out.push(abc[(a >> 2) as usize] as char);
            out.push(abc[((a & 3) << 4 | b >> 4) as usize] as char);
            out.push(if c.len() > 1 {
                abc[((b & 15) << 2 | d >> 6) as usize] as char
            } else {
                '='
            });
            out.push(if c.len() > 2 {
                abc[(d & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }
    fn png(color: [u8; 4]) -> String {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&color, 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        base64(&out)
    }
    fn read(reader: &mut BufReader<UnixStream>) -> Envelope {
        let mut b = Vec::new();
        assert_ne!(reader.read_until(b'\n', &mut b).unwrap(), 0);
        decode_line(&b).unwrap()
    }
    fn reply(stream: &mut UnixStream, id: &str, value: serde_json::Value) {
        let env = Envelope {
            v: IPC_VERSION.into(),
            id: id.into(),
            kind: FrameKind::Res,
            method: None,
            params: None,
            result: Some(value),
            error: None,
            seq: None,
        };
        stream.write_all(&encode_line(&env).unwrap()).unwrap();
    }
    fn frame(
        app: &mut PopupApp,
        ctx: &egui::Context,
        raster: &mut crate::runner::render::Rasterizer,
    ) -> Vec<u32> {
        frame_events(app, ctx, raster, Vec::new())
    }
    fn frame_events(
        app: &mut PopupApp,
        ctx: &egui::Context,
        raster: &mut crate::runner::render::Rasterizer,
        events: Vec<egui::Event>,
    ) -> Vec<u32> {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .focused = Some(true);
        let before = app
            .thumbnails
            .as_ref()
            .map_or(0, |thumbs| thumbs.worker.diagnostic_snapshot().len());
        let output = ctx.run_ui(input, |ui| {
            app.logic(ui.ctx(), WindowKind::Popup);
            app.ui(ui, WindowKind::Popup);
        });
        if let Some(thumbs) = &app.thumbnails {
            let events = thumbs.worker.diagnostic_snapshot();
            for event in events
                .iter()
                .skip(before)
                .filter(|event| event.event == "admitted")
            {
                assert!(
                    thumbs.slots.iter().any(|(id, _)| *id == event.id),
                    "request admitted outside current viewport: {}",
                    event.id
                );
            }
        }
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        raster.hold(output.textures_delta);
        let mut pixels = vec![0; 480 * 900];
        assert!(raster.paint(
            &mut pixels,
            480,
            900,
            output.pixels_per_point,
            egui::Color32::BLACK,
            &primitives
        ));
        pixels
    }
    // Test rectangles are within the positive raster viewport; fractional pixels truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn center(pixels: &[u32], rect: egui::Rect) -> u32 {
        let p = rect.center();
        pixels[p.y as usize * 480 + p.x as usize]
    }
    #[test]
    fn real_popup_renders_delayed_worker_pixels_in_row() {
        for hash_len in [16, 64] {
            render_delayed_worker_pixels(hash_len);
        }
    }
    #[allow(clippy::too_many_lines)] // The fixture checks one complete rendering sequence.
    fn render_delayed_worker_pixels(hash_len: usize) {
        let profile = Profile::new();
        let listener = UnixListener::bind(profile.0.join("clipmonger.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let red = clip(1, vec!["image/png"], &"a".repeat(hash_len));
        let text = clip(2, vec!["text/plain"], &"b".repeat(64));
        let id = red.id;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (dedup_tx, dedup_rx) = mpsc::sync_channel(1);
        let peer = Peer(Some(std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => {
                        // macOS: an accepted socket inherits the listener's O_NONBLOCK.
                        s.set_nonblocking(false).unwrap();
                        break s;
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                    {
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let hello = read(&mut reader);
            assert_eq!(hello.params.unwrap()["token"], "token");
            reply(&mut stream, "hello-0", serde_json::json!({"chosen":1}));
            let request = read(&mut reader);
            assert_eq!(request.params.unwrap()["clip"], id.to_string());
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            reply(
                &mut stream,
                "t0",
                serde_json::json!({"thumbnail":{"clip":id,"content_hash":"a".repeat(hash_len),"mime":"image/png","width":1,"height":1,"png_base64":png([255,0,0,255])}}),
            );
            let end = Instant::now() + Duration::from_millis(300);
            while Instant::now() < end {
                match listener.accept() {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::yield_now();
                    }
                    Ok(_) => panic!("duplicate request"),
                    Err(e) => panic!("accept: {e}"),
                }
            }
            dedup_tx.send(()).unwrap();
            let end = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => {
                        // macOS: an accepted socket inherits the listener's O_NONBLOCK.
                        s.set_nonblocking(false).unwrap();
                        break s;
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                    {
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let _hello = read(&mut reader);
            reply(&mut stream, "hello-0", serde_json::json!({"chosen":1}));
            let request = read(&mut reader);
            assert_eq!(request.params.unwrap()["clip"], id.to_string());
            reply(
                &mut stream,
                "t0",
                serde_json::json!({"thumbnail":{"clip":id,"content_hash":"a".repeat(hash_len),"mime":"image/png","width":1,"height":1,"png_base64":png([0,0,255,255])}}),
            );
        })));
        let mut release = Release(Some(release_tx));
        let (wake_tx, wake_rx) = mpsc::channel();
        let wake = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let worker = Worker::start(profile.0.clone(), wake).unwrap();
        let (cmds, _) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: profile.0.clone(),
            cmds,
            events,
            tray: Box::new(|_| Vec::new()),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        app.attach_thumbnails(worker);
        app.vm_mut().set_compact_rows(vec![
            crate::popup::CompactRow {
                id: red.id,
                pinned: false,
                preview: red.preview.clone(),
                kind: clipmonger_core::store::CompactKind::Image,
                thumbnail_hash: Some(red.content_hash.clone()),
            },
            crate::popup::CompactRow {
                id: text.id,
                pinned: false,
                preview: text.preview.clone(),
                kind: clipmonger_core::store::CompactKind::Text,
                thumbnail_hash: None,
            },
        ]);
        app.vm_mut().set_current(Some(red.clone()));
        let ctx = egui::Context::default();
        let mut raster = crate::runner::render::Rasterizer::new();
        let first = frame(&mut app, &ctx, &mut raster);
        let slots = app.thumbnails.as_ref().unwrap().slots.clone();
        let red_rects: Vec<_> = slots
            .iter()
            .filter(|(clip, _)| *clip == red.id)
            .map(|(_, rect)| *rect)
            .collect();
        assert_eq!(red_rects.len(), 1);
        assert_ne!(center(&first, red_rects[0]), 0xffff_0000);
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        release.0.take().unwrap().send(()).unwrap();
        wake_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("worker wake");
        let second = frame(&mut app, &ctx, &mut raster);
        let slots = app.thumbnails.as_ref().unwrap().slots.clone();
        let red_rects: Vec<_> = slots
            .iter()
            .filter(|(clip, _)| *clip == red.id)
            .map(|(_, rect)| *rect)
            .collect();
        assert_eq!(red_rects.len(), 1);
        assert_eq!(center(&second, red_rects[0]), 0xffff_0000);
        assert!(
            !slots.iter().any(|(clip, _)| *clip == text.id),
            "text rows get no thumbnail slot"
        );
        dedup_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("dedup observation");
        let other = egui::Context::default();
        let mut other_raster = crate::runner::render::Rasterizer::new();
        let before = frame(&mut app, &other, &mut other_raster);
        let slots = app.thumbnails.as_ref().unwrap().slots.clone();
        let current = slots.iter().find(|(clip, _)| *clip == red.id).unwrap().1;
        assert_ne!(center(&before, current), 0xffff_0000);
        wake_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("new context worker wake");
        let after = frame(&mut app, &other, &mut other_raster);
        let slots = app.thumbnails.as_ref().unwrap().slots.clone();
        let current = slots.iter().find(|(clip, _)| *clip == red.id).unwrap().1;
        assert_eq!(center(&after, current), 0xff00_00ff);
        events_tx
            .send(crate::ipc_bridge::UiEvent::Deleted(red.id))
            .unwrap();
        let _ = frame(&mut app, &other, &mut other_raster);
        assert!(app.vm().current().is_none());
        assert!(!app
            .thumbnails
            .as_ref()
            .unwrap()
            .slots
            .iter()
            .any(|(id, _)| *id == red.id));
        assert!(app.thumbnails.as_ref().unwrap().textures.is_empty());
        drop(app);
        peer.join();
    }
    #[test]
    fn changed_hash_cancels_pending_request_and_draws_new_pixels() {
        pending_replacement(false);
    }
    #[test]
    fn restore_cancels_pending_request_and_draws_new_pixels() {
        pending_replacement(true);
    }
    #[allow(clippy::too_many_lines)] // The fixture checks one complete rendering sequence.
    fn pending_replacement(restore: bool) {
        use std::io::Read;
        let profile = Profile::new();
        let listener = UnixListener::bind(profile.0.join("clipmonger.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let hash_len = if restore { 64 } else { 16 };
        let original = clip(1, vec!["image/png"], &"a".repeat(hash_len));
        let mut replacement = original.clone();
        replacement.content_hash = if restore { "a" } else { "b" }.repeat(hash_len);
        let target_hash = replacement.content_hash.clone();
        let id = original.id;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let peer = Peer(Some(std::thread::spawn(move || {
            let accept = |listener: &UnixListener| {
                let end = Instant::now() + Duration::from_secs(3);
                loop {
                    match listener.accept() {
                        Ok((s, _)) => {
                            // macOS: an accepted socket inherits the listener's O_NONBLOCK.
                            s.set_nonblocking(false).unwrap();
                            break s;
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < end =>
                        {
                            std::thread::yield_now();
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                }
            };
            let mut first = accept(&listener);
            first
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(first.try_clone().unwrap());
            let _ = read(&mut reader);
            reply(&mut first, "hello-0", serde_json::json!({"chosen":1}));
            let _ = read(&mut reader);
            ready_tx.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(reader.read(&mut byte).unwrap(), 0);
            let mut second = accept(&listener);
            second
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(second.try_clone().unwrap());
            let _ = read(&mut reader);
            reply(&mut second, "hello-0", serde_json::json!({"chosen":1}));
            let _ = read(&mut reader);
            reply(
                &mut second,
                "t0",
                serde_json::json!({"thumbnail":{"clip":id,"content_hash":target_hash,"mime":"image/png","width":1,"height":1,"png_base64":png([0,0,255,255])}}),
            );
        })));
        let (wake_tx, wake_rx) = mpsc::channel();
        let worker = Worker::start(
            profile.0.clone(),
            Arc::new(move || {
                let _ = wake_tx.send(());
            }),
        )
        .unwrap();
        let (cmds, cmd_rx) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: profile.0.clone(),
            cmds,
            events,
            tray: Box::new(|_| Vec::new()),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        app.attach_thumbnails(worker);
        app.vm_mut()
            .set_compact_rows(vec![crate::popup::CompactRow {
                id: original.id,
                pinned: false,
                preview: original.preview.clone(),
                kind: clipmonger_core::store::CompactKind::Image,
                thumbnail_hash: Some(original.content_hash.clone()),
            }]);
        app.vm_mut().set_current(Some(original.clone()));
        let ctx = egui::Context::default();
        let mut raster = crate::runner::render::Rasterizer::new();
        let _ = frame(&mut app, &ctx, &mut raster);
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let initial_commands: Vec<_> = cmd_rx.try_iter().collect();
        let initial = initial_commands
            .iter()
            .find_map(|cmd| match cmd {
                crate::ipc_bridge::UiCmd::CompactHistory(tag) => Some(tag.clone()),
                _ => None,
            })
            .unwrap();
        let initial_current = initial_commands
            .iter()
            .find_map(|cmd| match cmd {
                crate::ipc_bridge::UiCmd::CompactCurrent(tag) => Some(*tag),
                _ => None,
            })
            .unwrap();
        events_tx
            .send(crate::ipc_bridge::UiEvent::CompactCurrent(
                initial_current,
                Ok(serde_json::json!({"clips": [original]})),
            ))
            .unwrap();
        let _ = frame(&mut app, &ctx, &mut raster);
        if !restore {
            app.refresh_current();
        }
        let make_reply = |tag: &crate::ipc_bridge::CompactRequest, rows| {
            crate::ipc_bridge::UiEvent::CompactHistory(
                tag.clone(),
                Ok(
                    serde_json::json!({"owner":tag.params.owner,"generation":tag.params.generation,"rows":rows,"cursor":null,"done":true,"bytes_read":0,"candidates":1}),
                ),
            )
        };
        if restore {
            events_tx
                .send(crate::ipc_bridge::UiEvent::RestoreDone { warning: None })
                .unwrap();
            events_tx
                .send(make_reply(&initial, serde_json::json!([])))
                .unwrap();
            let _ = frame(&mut app, &ctx, &mut raster);
        }
        let commands: Vec<_> = cmd_rx.try_iter().collect();
        let tag = if restore {
            commands
                .iter()
                .find_map(|cmd| match cmd {
                    crate::ipc_bridge::UiCmd::CompactHistory(tag) => Some(tag.clone()),
                    _ => None,
                })
                .unwrap()
        } else {
            initial
        };
        let current_tag = commands
            .iter()
            .find_map(|cmd| match cmd {
                crate::ipc_bridge::UiCmd::CompactCurrent(tag) => Some(*tag),
                _ => None,
            })
            .expect("current lookup");
        let row = serde_json::json!({"id":replacement.id,"pinned":false,"last_used_ms":1,"preview":replacement.preview,"preview_truncated":false,"kind":"image","thumbnail_hash":replacement.content_hash});
        events_tx
            .send(make_reply(&tag, serde_json::json!([row])))
            .unwrap();
        events_tx
            .send(crate::ipc_bridge::UiEvent::CompactCurrent(
                current_tag,
                Ok(serde_json::json!({"clips": [replacement]})),
            ))
            .unwrap();
        let interim = frame(&mut app, &ctx, &mut raster);
        let rect = app.thumbnails.as_ref().unwrap().slots[0].1;
        assert_ne!(center(&interim, rect), 0xffff_0000);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            wake_rx.recv_timeout(remaining).expect("replacement wake");
            let final_pixels = frame(&mut app, &ctx, &mut raster);
            let rect = app.thumbnails.as_ref().unwrap().slots[0].1;
            if center(&final_pixels, rect) == 0xff00_00ff {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "replacement pixels did not arrive"
            );
        }
        drop(app);
        peer.join();
    }
    // Every repeated peer request must follow an actual full-result-channel drop
    // before the subsequent admission for the same key and epoch.
    fn collect_requests(
        app: &PopupApp,
        peer: &mpsc::Receiver<uuid::Uuid>,
        allowed: &HashSet<uuid::Uuid>,
        seen: &mut HashSet<uuid::Uuid>,
        attempts: &mut HashMap<uuid::Uuid, (usize, u64)>,
    ) {
        while let Ok(id) = peer.try_recv() {
            assert!(allowed.contains(&id), "non-visible request: {id}");
            let events = app
                .thumbnails
                .as_ref()
                .unwrap()
                .worker
                .diagnostic_snapshot();
            assert!(events.len() < 1024, "worker observation capacity exceeded");
            let start = attempts.get(&id).map_or(0, |(index, _)| index + 1);
            let (index, current) = events
                .iter()
                .enumerate()
                .skip(start)
                .find(|(_, event)| event.event == "admitted" && event.id == id)
                .expect("peer request must have an observed queue admission");
            assert!(
                events[index + 1..]
                    .iter()
                    .any(|event| event.event == "active_started"
                        && event.id == id
                        && event.epoch == current.epoch),
                "admitted peer request must have started"
            );
            if let Some((previous, epoch)) = attempts.get(&id) {
                let between = &events[*previous + 1..index];
                let dropped = between.iter().position(|event| {
                    event.event == "result_dropped_full" && event.id == id && event.epoch == *epoch
                });
                assert!(
                    dropped.is_some(),
                    "duplicate admitted before a matching drop for {id}: {events:?}"
                );
                // A resubmit of the dropped epoch that meets a full queue is
                // deferred and abandoned, so the next admission mints a new
                // epoch; any other epoch change is a stray request.
                if current.epoch != *epoch {
                    assert!(
                        between[dropped.unwrap_or(0)..].iter().any(|event| {
                            event.event == "deferred" && event.id == id && event.epoch == *epoch
                        }),
                        "repeat must keep the dropped epoch unless it was deferred for {id}: {events:?}"
                    );
                }
            }
            attempts.insert(id, (index, current.epoch));
            seen.insert(id);
        }
    }
    #[allow(clippy::too_many_arguments)] // Shared viewport fixture keeps its observation state explicit.
    fn settle_viewport(
        app: &mut PopupApp,
        ctx: &egui::Context,
        raster: &mut crate::runner::render::Rasterizer,
        peer: &mpsc::Receiver<uuid::Uuid>,
        wakes: &mpsc::Receiver<()>,
        expected: &HashSet<uuid::Uuid>,
        allowed: &HashSet<uuid::Uuid>,
        seen: &mut HashSet<uuid::Uuid>,
        attempts: &mut HashMap<uuid::Uuid, (usize, u64)>,
    ) {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            collect_requests(app, peer, allowed, seen, attempts);
            let keys: Vec<_> = app
                .vm()
                .clips()
                .iter()
                .filter(|clip| expected.contains(&clip.id))
                .map(|clip| {
                    app.thumbnails
                        .as_ref()
                        .unwrap()
                        .key(clip.id, Some(&clip.content_hash))
                        .unwrap()
                })
                .collect();
            assert_eq!(keys.len(), expected.len());
            let ready = {
                let thumbs = app.thumbnails.as_mut().unwrap();
                keys.iter()
                    .all(|key| thumbs.cache.state(key, Instant::now()) == CacheState::Ready)
            };
            if ready {
                collect_requests(app, peer, allowed, seen, attempts);
                assert!(
                    expected.iter().all(|id| seen.contains(id)),
                    "visible item never reached peer"
                );
                break;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("viewport did not settle");
            wakes.recv_timeout(remaining).expect("completion wake");
            let _ = frame(app, ctx, raster);
        }
    }
    #[test]
    #[allow(clippy::too_many_lines)] // The fixture checks one complete rendering sequence.
    fn scroll_requests_only_intersecting_rows_and_retries_deferred_work() {
        struct Stop(Option<mpsc::SyncSender<()>>);
        impl Drop for Stop {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.try_send(());
                }
            }
        }
        let profile = Profile::new();
        let listener = UnixListener::bind(profile.0.join("clipmonger.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);
        let (gate_tx, gate_rx) = mpsc::sync_channel(1);
        let peer = Peer(Some(std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(8);
            let mut first = true;
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS: an accepted socket inherits the listener's O_NONBLOCK.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut line = Vec::new();
                        if !matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                            continue;
                        }
                        assert_eq!(decode_line(&line).unwrap().method.as_deref(), Some("hello"));
                        let welcome = Envelope {
                            v: IPC_VERSION.into(),
                            id: "hello-0".into(),
                            kind: FrameKind::Res,
                            method: None,
                            params: None,
                            result: Some(serde_json::json!({"chosen":1})),
                            error: None,
                            seq: None,
                        };
                        if stream.write_all(&encode_line(&welcome).unwrap()).is_err() {
                            continue;
                        }
                        line.clear();
                        if !matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                            continue;
                        }
                        let request = decode_line(&line).unwrap();
                        let id: uuid::Uuid =
                            serde_json::from_value(request.params.unwrap()["clip"].clone())
                                .unwrap();
                        seen_tx.send(id).unwrap();
                        if first {
                            first = false;
                            gate_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                        }
                        let response = Envelope {
                            v: IPC_VERSION.into(),
                            id: "t0".into(),
                            kind: FrameKind::Res,
                            method: None,
                            params: None,
                            result: Some(serde_json::json!({"thumbnail":null})),
                            error: None,
                            seq: None,
                        };
                        let _ = stream.write_all(&encode_line(&response).unwrap());
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                    {
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        })));
        let mut stop = Stop(Some(stop_tx));
        let mut gate = Release(Some(gate_tx));
        let (wake_tx, wake_rx) = mpsc::channel();
        let worker = Worker::start(
            profile.0.clone(),
            Arc::new(move || {
                let _ = wake_tx.send(());
            }),
        )
        .unwrap();
        let (cmds, _) = mpsc::channel();
        let (_event_tx, events) = mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: profile.0.clone(),
            cmds,
            events,
            tray: Box::new(|_| Vec::new()),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        app.attach_thumbnails(worker);
        app.thumbnails.as_ref().unwrap().worker.diagnostic_enable();
        app.vm_mut().set_clips(
            (1..=30)
                .map(|i| clip(i, vec!["image/png"], &"a".repeat(64)))
                .collect(),
        );
        let ctx = egui::Context::default();
        let mut raster = crate::runner::render::Rasterizer::new();
        let _ = frame(&mut app, &ctx, &mut raster);
        let initial: HashSet<_> = app
            .thumbnails
            .as_ref()
            .unwrap()
            .slots
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert!(initial.len() > 9 && initial.len() < 30);
        assert!(
            app.thumbnails.as_ref().unwrap().deferred,
            "viewport must exercise queue deferral"
        );
        gate.0.take().unwrap().send(()).unwrap();
        let mut seen = HashSet::new();
        let mut attempts = HashMap::new();
        settle_viewport(
            &mut app,
            &ctx,
            &mut raster,
            &seen_rx,
            &wake_rx,
            &initial,
            &initial,
            &mut seen,
            &mut attempts,
        );
        let thumbs = app.thumbnails.as_ref().unwrap();
        let (viewport, before_offset) = thumbs.history_area.expect("history viewport");
        let clipped = thumbs
            .slots
            .iter()
            .map(|(_, rect)| rect.intersect(viewport))
            .find(|rect| rect.is_positive() && viewport.contains(rect.center()))
            .expect("visible slot inside history viewport");
        let pointer = clipped.center();
        assert!(viewport.contains(pointer));
        let _ = frame_events(
            &mut app,
            &ctx,
            &mut raster,
            vec![egui::Event::PointerMoved(pointer)],
        );
        assert_eq!(ctx.input(|input| input.pointer.hover_pos()), Some(pointer));
        let _ = frame_events(
            &mut app,
            &ctx,
            &mut raster,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                source: egui::MouseWheelSource::Unknown,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0.0, -350.0),
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let mut allowed = initial.clone();
        let mut previous = before_offset.y;
        let mut stable = 0;
        let mut moved = false;
        for _ in 0..60 {
            let _ = frame(&mut app, &ctx, &mut raster);
            let thumbs = app.thumbnails.as_ref().unwrap();
            allowed.extend(thumbs.slots.iter().map(|(id, _)| *id));
            let offset = thumbs
                .history_area
                .expect("history viewport after wheel")
                .1
                .y;
            moved |= offset > before_offset.y + 0.1;
            if moved && (offset - previous).abs() < 0.01 {
                stable += 1;
            } else {
                stable = 0;
            }
            previous = offset;
            if stable >= 3 {
                break;
            }
        }
        assert!(moved, "wheel did not advance the history offset");
        assert!(
            stable >= 3,
            "wheel scrolling did not settle within the frame bound"
        );
        let scrolled: HashSet<_> = app
            .thumbnails
            .as_ref()
            .unwrap()
            .slots
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let new: HashSet<_> = scrolled.difference(&initial).copied().collect();
        assert!(!new.is_empty(), "scroll must expose new rows");
        settle_viewport(
            &mut app,
            &ctx,
            &mut raster,
            &seen_rx,
            &wake_rx,
            &new,
            &allowed,
            &mut seen,
            &mut attempts,
        );
        assert!(seen_rx.try_recv().is_err());
        stop.0.take().unwrap().send(()).unwrap();
        drop(app);
        peer.join();
    }
    #[test]
    fn visible_cooldown_schedules_repaint_and_admits_retry() {
        use crate::thumbnail_worker::{Completion, Failure};

        let profile = Profile::new();
        let worker = Worker::start(profile.0.clone(), Arc::new(|| {})).unwrap();
        worker.diagnostic_enable();
        let mut thumbs = Thumbnails::new(worker);
        let ctx = egui::Context::default();
        // Settle egui's initial repaint requests before measuring this frame.
        for _ in 0..3 {
            ctx.run_ui(egui::RawInput::default(), |_| {})
                .textures_delta
                .clear();
        }
        let key = thumbs
            .key(uuid::Uuid::from_u128(1), Some(&"a".repeat(64)))
            .unwrap();
        thumbs.cache.set_visible([key.clone()]);
        let now = Instant::now();
        let request = thumbs.cache.begin(&key, now).unwrap();
        assert!(thumbs.cache.accept(
            Completion {
                request,
                result: Err(Failure::Retryable)
            },
            now
        ));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            thumbs.update(ui.ctx(), vec![key.clone()]);
        });
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.textures_delta.clear();
        assert!(
            delay > Duration::from_secs(1) && delay <= Duration::from_secs(2),
            "unexpected retry repaint delay: {delay:?}"
        );
        std::thread::sleep(delay + Duration::from_millis(20));
        ctx.run_ui(egui::RawInput::default(), |ui| {
            thumbs.update(ui.ctx(), vec![key.clone()]);
        })
        .textures_delta
        .clear();
        assert!(thumbs
            .worker
            .diagnostic_snapshot()
            .iter()
            .any(|event| event.event == "admitted" && event.id == key.clip));
    }
    #[test]
    fn retry_wake_uses_earliest_visible_deadline_and_ignores_terminal_entries() {
        use crate::thumbnail_worker::{Completion, Failure};

        let profile = Profile::new();
        let worker = Worker::start(profile.0.clone(), Arc::new(|| {})).unwrap();
        worker.diagnostic_enable();
        let mut thumbs = Thumbnails::new(worker);
        let keys: Vec<_> = (1..=4)
            .map(|id| {
                thumbs
                    .key(uuid::Uuid::from_u128(id), Some(&"a".repeat(64)))
                    .unwrap()
            })
            .collect();
        thumbs.cache.set_visible(keys.clone());
        let now = Instant::now();
        for (index, key) in keys.iter().enumerate() {
            let request = thumbs.cache.begin(key, now).unwrap();
            let failure = if index == 3 {
                Failure::Invalid
            } else {
                Failure::Retryable
            };
            // Key 2 (hidden) cools down first, then visible key 1, then key 0.
            let accepted_at = match index {
                1 => now.checked_sub(Duration::from_millis(500)).unwrap(),
                2 => now.checked_sub(Duration::from_secs(1)).unwrap(),
                _ => now,
            };
            assert!(thumbs.cache.accept(
                Completion {
                    request,
                    result: Err(failure)
                },
                accepted_at
            ));
        }
        let earliest = thumbs
            .cache
            .retry_deadline(&keys[1], now)
            .expect("visible key 1 cools down");
        let later = thumbs
            .cache
            .retry_deadline(&keys[0], now)
            .expect("visible key 0 cools down");
        assert!(earliest < later);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            ctx.run_ui(egui::RawInput::default(), |_| {})
                .textures_delta
                .clear();
        }
        // Bound the wake by the clock around the pass instead of a fixed
        // window: a loaded run can spend hundreds of milliseconds before it.
        let before = Instant::now();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            thumbs.update(
                ui.ctx(),
                vec![keys[0].clone(), keys[1].clone(), keys[3].clone()],
            );
        });
        let after = Instant::now();
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.textures_delta.clear();
        let slack = Duration::from_millis(5);
        assert!(
            delay + slack >= earliest.saturating_duration_since(after)
                && delay <= earliest.saturating_duration_since(before) + slack,
            "earliest visible delay: {delay:?}, deadline in {:?}",
            earliest.saturating_duration_since(before)
        );
        // The remaining retry entry is hidden, and the visible invalid entry is terminal.
        let other = egui::Context::default();
        for _ in 0..3 {
            other
                .run_ui(egui::RawInput::default(), |_| {})
                .textures_delta
                .clear();
        }
        let mut output = other.run_ui(egui::RawInput::default(), |ui| {
            thumbs.update(ui.ctx(), vec![keys[3].clone()]);
        });
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.textures_delta.clear();
        assert_eq!(delay, Duration::MAX);
        assert!(thumbs.worker.diagnostic_snapshot().is_empty());
    }
    #[test]
    fn missing_peer_renders_fixed_placeholder() {
        let profile = Profile::new();
        let image = clip(1, vec!["image/png"], &"a".repeat(64));
        let (wake_tx, wake_rx) = mpsc::channel();
        let worker = Worker::start(
            profile.0.clone(),
            Arc::new(move || {
                let _ = wake_tx.send(());
            }),
        )
        .unwrap();
        let (cmds, _) = mpsc::channel();
        let (_event_tx, events) = mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: profile.0.clone(),
            cmds,
            events,
            tray: Box::new(|_| Vec::new()),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        app.attach_thumbnails(worker);
        app.vm_mut().set_clips(vec![image.clone()]);
        let ctx = egui::Context::default();
        let mut raster = crate::runner::render::Rasterizer::new();
        let _ = frame(&mut app, &ctx, &mut raster);
        wake_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("failed fetch wake");
        let pixels = frame(&mut app, &ctx, &mut raster);
        let rect = app.thumbnails.as_ref().unwrap().slots[0].1;
        assert_ne!(center(&pixels, rect), 0xffff_0000);
        assert!(app.thumbnails.as_ref().unwrap().textures.is_empty());
    }
}
