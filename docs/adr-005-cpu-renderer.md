# ADR-005 — One egui UI on the CPU renderer

- Status: implemented in source; native acceptance remains pending.
- Decision: D-027, amending D-004 and retiring the D-024 Win32 fallback.
- Date: 2026-11-03.

## Context and decision

The earlier UI used eframe with a GPU renderer and a separate Windows GDI
fallback for remote and virtual displays. The current shell uses egui and
egui-winit with an application-owned winit event loop on every platform.
The hdrshot-egui-backend software rasterizer paints CPU buffers, which
softbuffer presents. No OpenGL or Vulkan renderer is required.

The runner owns a popup and an optional Settings window, each with its own
egui context and rasterizer. Application logic handles state changes while
hidden or minimized; drawing happens only when a window can be presented.
Texture updates from hidden passes are retained for the next draw.
Closing Settings does not terminate the application.

Windows and X11 retain the popup window. Wayland creates it for a show and
delegates placement to the compositor. An available activation token is
applied to the newly created window; X11 carries the triggering timestamp.
Actual native focus and activation behavior remains subject to QA.

## Popup context

The daemon owns global hotkeys and press-bound gestures. The UI registers
its popup identity where available and applies these shell transitions:

| Event | Popup behavior | Paste context |
|---|---|---|
| Non-toggle hotkey push | Open or raise | Replace with that press's target and gesture |
| Toggle push while open | Hide | Clear |
| Toggle push while closed | Stay hidden | Unchanged |
| Request while hidden | Open | Empty |
| Request while already open | Raise | Preserve the current open context |
| Hide, including focus loss | Hide | Clear |
| Paste failure after hiding | Reopen with error | Empty |

Paste captures context before hiding. Tray and CLI requests do not mint an
elevated-paste gesture. A second launch hands over to the existing UI for the
profile. Minimized startup does not show the popup.

## Build and fonts

The GUI declares Rust 1.95 because egui, egui-winit and
egui_system_font_provider 0.37, and hdrshot-egui-backend 0.2 require it.
The foundation workspace's Rust 1.89 declaration is not the GUI compiler
floor. The pinned build toolchain is separate from package MSRV declarations.
The GUI's explicit edition and license values match the workspace.

System and bundled fallback fonts serve the same UI across backends.
Release packaging must include the complete font notices and generated Rust
dependency notices. See [the font inventory](../licenses/fonts/README.md)
for optional-feature fonts.

## Verification and limits

Software controls cover shell transitions, hidden scheduling, rendering,
placement, input, IPC and failure recovery. Windows GNU checks compile and
lint the Windows code; they do not establish native behavior.

Run [T-905](../tests/manual/T-905-window-matrix.md),
[T-906](../tests/manual/T-906-inventory-walk.md),
[T-907](../tests/manual/T-907-keyboard-only.md) and
[T-908](../tests/manual/T-908-focus-interplay.md) on the current runner.
Record RustDesk and physical-display, DPI, multi-monitor, compositor and
focus results. Earlier dated renderer verdicts retain their original scope.
Screen-reader support has not been established for this owned runner.

## Source

- [Runner contract](../crates/app-ui/src/runner/mod.rs)
- [Rasterization and presentation](../crates/app-ui/src/runner/render.rs)
- [Shell transitions](../crates/app-ui/src/shell.rs)
- [GUI dependency declarations](../crates/app-ui/Cargo.toml)
