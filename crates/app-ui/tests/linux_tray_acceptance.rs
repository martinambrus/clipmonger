//! Linux tray menu mapping acceptance: `linux_tray` pure model roundtrip.

use clipmonger_app_ui::{linux_tray, strings, tray_logic};
use clipmonger_platform::fakes::FakeTray;
use clipmonger_platform::types::TrayEvent;

fn inputs_for(paused: bool) -> tray_logic::TrayInputs {
    tray_logic::TrayInputs {
        paused,
        degraded: false,
        attached: true,
    }
}

#[test]
fn linux_menu_covers_every_entry_with_roundtrip_actions() {
    for paused in [false, true] {
        let inputs = inputs_for(paused);
        let rows = linux_tray::linux_menu(&inputs);
        let entries = tray_logic::menu_entries(&inputs);
        assert_eq!(rows.len(), entries.len());
        for (row, entry) in rows.iter().zip(entries.iter()) {
            match (row, entry) {
                (linux_tray::LinuxMenuRow::Item(item), tray_logic::MenuEntry::Item(id)) => {
                    assert_eq!(item.id, *id);
                    assert_eq!(item.label, strings::text(id));
                    let mut logic = tray_logic::TrayLogic::new();
                    let mut tray = FakeTray::new(vec![TrayEvent::MenuItem(item.label.clone())]);
                    assert_eq!(
                        logic.on_events(&mut tray),
                        vec![item.action.clone()],
                        "linux row {id:?} must roundtrip through TrayLogic label mapping"
                    );
                }
                (linux_tray::LinuxMenuRow::Separator, tray_logic::MenuEntry::Separator) => {}
                _ => panic!("linux row {row:?} must mirror entry {entry:?}"),
            }
        }
    }
}

#[test]
fn linux_menu_swaps_pause_label_when_paused() {
    let rows = linux_tray::linux_menu(&inputs_for(false));
    let ids: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            linux_tray::LinuxMenuRow::Item(item) => Some(item.id),
            linux_tray::LinuxMenuRow::Separator => None,
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            "tray.show",
            "tray.pause",
            "tray.pause_1h",
            "tray.config",
            "tray.about",
            "tray.quit",
        ]
    );
    let paused_rows = linux_tray::linux_menu(&inputs_for(true));
    let paused_ids: Vec<&str> = paused_rows
        .iter()
        .filter_map(|row| match row {
            linux_tray::LinuxMenuRow::Item(item) => Some(item.id),
            linux_tray::LinuxMenuRow::Separator => None,
        })
        .collect();
    assert!(paused_ids.contains(&"tray.resume"));
    assert!(!paused_ids.contains(&"tray.pause"));
    assert_eq!(paused_rows.len(), rows.len());
}

#[test]
fn linux_action_for_id_matches_shell_dispatch() {
    assert_eq!(
        linux_tray::action_for_id("tray.show"),
        tray_logic::TrayAction::OpenPopup
    );
    assert_eq!(
        linux_tray::action_for_id("tray.pause"),
        tray_logic::TrayAction::TogglePause
    );
    assert_eq!(
        linux_tray::action_for_id("tray.resume"),
        tray_logic::TrayAction::TogglePause
    );
    assert_eq!(
        linux_tray::action_for_id("tray.pause_1h"),
        tray_logic::TrayAction::PauseFor1h
    );
    assert_eq!(
        linux_tray::action_for_id("tray.config"),
        tray_logic::TrayAction::OpenConfig
    );
    assert_eq!(
        linux_tray::action_for_id("tray.about"),
        tray_logic::TrayAction::About
    );
    assert_eq!(
        linux_tray::action_for_id("tray.quit"),
        tray_logic::TrayAction::Quit
    );
    assert_eq!(
        linux_tray::action_for_id("tray.unknown"),
        tray_logic::TrayAction::Quit
    );
}

#[test]
fn linux_icon_pixmaps_are_square_argb_and_differ_by_state() {
    let normal = linux_tray::icon_pixmaps(&inputs_for(false));
    assert_eq!(
        normal.iter().map(|(edge, _)| *edge).collect::<Vec<_>>(),
        [16, 20, 22, 24, 32, 48]
    );
    for (edge, data) in &normal {
        let edge = usize::try_from(*edge).unwrap();
        assert_eq!(data.len(), edge * edge * 4);
    }
    let (edge, data) = &normal[0];
    let edge = usize::try_from(*edge).unwrap();
    let corner = &data[..4];
    let centre = &data[(edge * edge / 2 + edge / 2) * 4..][..4];
    assert!(corner[0] < 64, "see-through corner, alpha comes first");
    assert_eq!(centre[0], 0xff, "opaque middle");
    let paused = linux_tray::icon_pixmaps(&inputs_for(true));
    assert_eq!(paused.len(), normal.len());
    assert_ne!(paused, normal);
    let degraded = linux_tray::icon_pixmaps(&tray_logic::TrayInputs {
        paused: false,
        degraded: true,
        attached: true,
    });
    assert_ne!(degraded, normal);
    assert_ne!(degraded, paused);
    let detached = linux_tray::icon_pixmaps(&tray_logic::TrayInputs {
        paused: false,
        degraded: false,
        attached: false,
    });
    assert_eq!(detached, degraded);
}

#[test]
fn tray_icon_picks_the_smallest_size_that_fits() {
    let state = tray_logic::TrayUiState::Normal;
    assert_eq!(tray_logic::icon_rgba(state, 16).0, 16);
    assert_eq!(tray_logic::icon_rgba(state, 18).0, 20);
    assert_eq!(tray_logic::icon_rgba(state, 25).0, 32);
    assert_eq!(tray_logic::icon_rgba(state, 96).0, 48);
}
