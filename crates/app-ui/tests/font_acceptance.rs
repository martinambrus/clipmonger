//! Font regression: eframe is built with `default-features = false`, which
//! once dropped `default_fonts`/`system_fonts` and rendered every label as
//! tofu squares on a real display (observed Ubuntu 24.04, 2026-10-27).

#[test]
fn default_font_definitions_carry_real_fonts() {
    let defs = egui::FontDefinitions::default();
    assert!(
        !defs.font_data.is_empty(),
        "no embedded/system fonts: all UI text renders as squares"
    );
    let proportional = defs
        .families
        .get(&egui::FontFamily::Proportional)
        .expect("proportional family defined");
    assert!(
        !proportional.is_empty(),
        "proportional family has no font faces"
    );
    for name in proportional {
        assert!(
            defs.font_data.contains_key(name),
            "family references missing font data: {name}"
        );
    }
}
