//! Frame cost of a popup-like ui on the CPU rasterizer: search box, a list of
//! rows and a hints line, 480x360 points at 2.0 pixels per point.
//!
//! Timing needs an optimised build, so the test is ignored by default:
//! `cargo test --release -p clipmonger-app-ui --test runner_perf -- --ignored --nocapture`

use std::time::{Duration, Instant};

use clipmonger_app_ui::runner::render::Rasterizer;
use egui::{Color32, Context, FullOutput, Pos2, RawInput, Rect, Ui, Vec2};

const POINTS: [f32; 2] = [480.0, 360.0];
const PIXELS_PER_POINT: f32 = 2.0;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 720;
const WARMUP_FRAMES: usize = 20;
const MEASURED_FRAMES: usize = 300;
const BUDGET_P90: Duration = Duration::from_millis(2);
const CLEAR: Color32 = Color32::from_rgb(27, 27, 27);
const TYPED: &str = "invoice march";
const ROWS: [&str; 14] = [
    "Meeting notes for the quarterly planning session on Thursday",
    "https://example.org/docs/getting-started#installation",
    "SELECT id, created_ms, preview FROM clips ORDER BY created_ms DESC",
    "ssh deploy@build-04.internal -p 2222",
    "Invoice 2026-0317 is attached, payment due within 14 days",
    "C:\\Users\\anna\\Documents\\Reports\\March\\summary.xlsx",
    "The quick brown fox jumps over the lazy dog",
    "fn main() { println!(\"hello\"); }",
    "Call back +420 123 456 789 after lunch",
    "Remember to renew the domain before the end of the month",
    "anna.novak@example.org",
    "git rebase --onto main feature~3 feature",
    "Shopping list: milk, eggs, flour, coffee, oranges",
    "Dear team, please find the updated schedule below",
];

fn input() -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(POINTS[0], POINTS[1]),
        )),
        ..RawInput::default()
    }
}

fn popup_ui(ui: &mut Ui, query: &mut String, selected: usize) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.add(
            egui::TextEdit::singleline(query)
                .hint_text("Search clips")
                .desired_width(f32::INFINITY),
        );
        egui::ScrollArea::vertical()
            .max_height(POINTS[1] - 70.0)
            .show(ui, |ui| {
                for (row, text) in ROWS.iter().enumerate() {
                    let _ = ui.selectable_label(row == selected, *text);
                }
            });
        ui.label("Up/Down move   Enter paste   Esc close   Ctrl+P pin");
    });
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    sorted[sorted.len() * percent / 100]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[test]
#[ignore = "timing: run in release with `-- --ignored --nocapture`"]
fn popup_frame_cost_stays_within_budget() {
    let optimised = !cfg!(debug_assertions);
    assert!(
        optimised,
        "timing needs an optimised build: run with --release"
    );
    let ctx = Context::default();
    ctx.set_pixels_per_point(PIXELS_PER_POINT);
    let mut rasterizer = Rasterizer::new();
    let mut pixels = vec![0_u32; usize::try_from(WIDTH * HEIGHT).expect("buffer size")];
    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    for frame in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let mut query = TYPED[..=frame % (TYPED.len() - 1)].to_owned();
        let selected = frame % ROWS.len();
        let started = Instant::now();
        let FullOutput {
            textures_delta,
            shapes,
            pixels_per_point,
            ..
        } = ctx.run_ui(input(), |ui| popup_ui(ui, &mut query, selected));
        let primitives = ctx.tessellate(shapes, pixels_per_point);
        rasterizer.hold(textures_delta);
        let painted = rasterizer.paint(
            &mut pixels,
            WIDTH,
            HEIGHT,
            pixels_per_point,
            CLEAR,
            &primitives,
        );
        let elapsed = started.elapsed();
        assert!(painted, "frame {frame} was not painted");
        if frame >= WARMUP_FRAMES {
            assert!(
                (pixels_per_point - PIXELS_PER_POINT).abs() < 1.0e-6,
                "pixels per point {pixels_per_point}"
            );
            samples.push(elapsed);
        }
    }
    let clear_word = 0xFF00_0000
        | (u32::from(CLEAR.r()) << 16)
        | (u32::from(CLEAR.g()) << 8)
        | u32::from(CLEAR.b());
    let lit = pixels.iter().filter(|&&p| p != clear_word).count();
    assert!(
        lit > pixels.len() / 20,
        "the ui drew almost nothing: {lit} pixels"
    );
    samples.sort();
    let (p50, p90, p99) = (
        percentile(&samples, 50),
        percentile(&samples, 90),
        percentile(&samples, 99),
    );
    println!(
        "runner_perf {WIDTH}x{HEIGHT} px, {MEASURED_FRAMES} frames after {WARMUP_FRAMES} warm-up: \
         p50 {:.3} ms, p90 {:.3} ms, p99 {:.3} ms (budget p90 < {:.1} ms)",
        millis(p50),
        millis(p90),
        millis(p99),
        millis(BUDGET_P90),
    );
    assert!(
        p90 < BUDGET_P90,
        "p90 {:.3} ms is over the {:.1} ms budget",
        millis(p90),
        millis(BUDGET_P90)
    );
}
