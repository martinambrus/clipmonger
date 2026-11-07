# KDE Wayland protocol bindings (Linux)

Two Linux binaries contain Rust bindings that `wayland-protocols-plasma`
0.3.12 (manifest: MIT) generates at build time from KDE protocol descriptions:

- The GUI (`clipmonger`) uses winit 0.30 for its windows. winit's Wayland
  backend always depends on `wayland-protocols-plasma` and compiles the
  bindings for KDE's `blur.xml`.
- The daemon (`clipmonger-daemon`) compiles the bindings for KDE's
  `fake-input.xml`, which it uses to press the paste chord on KDE Plasma.

Both protocol files carry their own license:

    blur.xml:
    SPDX-FileCopyrightText: 2015 Martin Gräßlin
    SPDX-FileCopyrightText: 2015 Marco Martin
    SPDX-License-Identifier: LGPL-2.1-or-later

    fake-input.xml:
    SPDX-FileCopyrightText: 2015 Martin Gräßlin
    SPDX-License-Identifier: LGPL-2.1-or-later

Clipmonger treats the generated bindings as covered by that license. Those
portions of the Linux binaries are distributed under the GNU Lesser General
Public License version 2.1 or later; its full text is in `LGPL-2.1.txt`. The
rest of Clipmonger stays under the MIT license in `LICENSE`.

The binaries are statically linked. To meet LGPL-2.1 section 6, the complete
corresponding source is public: Clipmonger's own source at its repository, and
`wayland-protocols-plasma` 0.3.12 (with `blur.xml` and `fake-input.xml`) on
crates.io. You may change the protocol bindings or the crate and rebuild the
binaries from that source with `cargo build --release`, using
`[patch.crates-io]` to point at your copy.

Windows builds do not include these bindings.

## Permissively licensed protocol bindings

For auto-paste on wlroots-family compositors (Sway, Hyprland) and COSMIC the
daemon also compiles bindings for more protocol descriptions. All carry
permissive licenses that need no source offer, only the notice:

    virtual-keyboard-unstable-v1.xml (wayland-protocols-misc 0.3.12, manifest: MIT):
    Copyright © 2008-2011 Kristian Høgsberg
    Copyright © 2010-2013 Intel Corporation
    Copyright © 2012-2013 Collabora, Ltd.
    Copyright © 2018 Purism SPC
    License: MIT (the notice is in the protocol file)

    wlr-foreign-toplevel-management-unstable-v1.xml (wayland-protocols-wlr 0.3.12, manifest: MIT):
    Copyright © 2018 Ilia Bozhinov
    License: HPND-style permission notice (in the protocol file)

    ext-foreign-toplevel-list-v1.xml (wayland-protocols 0.32.13, staging, manifest: MIT):
    Copyright © 2018 Ilia Bozhinov
    Copyright © 2020 Isaac Freund
    Copyright © 2022 wb9688
    Copyright © 2023 i509VCB
    License: HPND-style permission notice (in the protocol file)

    ext-workspace-v1.xml (wayland-protocols 0.32.13, staging, manifest: MIT;
    referenced by the COSMIC protocol below):
    Copyright © 2019 Christopher Billington
    Copyright © 2020 Ilia Bozhinov
    Copyright © 2022 Victoria Brekenfeld
    License: HPND-style permission notice (in the protocol file)

    cosmic-toplevel-info-unstable-v1.xml (vendored in
    crates/backend-wayland/protocols/ from pop-os/cosmic-protocols, commit
    c0cff4db14c37ed954983158e4055aa94c7741d9, unmodified):
    Copyright © 2018 Ilia Bozhinov
    Copyright © 2020 Isaac Freund
    Copyright © 2024 Victoria Brekenfeld
    License: HPND-style permission notice (in the protocol file)

The `cosmic-protocols` crate itself is not used (its crates.io release is
labelled GPL-3.0-only); Clipmonger generates the bindings from the vendored
file with `wayland-scanner`. `protocols/cosmic-workspace-stub.xml` next to it
is Clipmonger's own (MIT): an empty stand-in for the
`zcosmic_workspace_handle_v1` interface the COSMIC file names.

Linux release packages (`.deb`, portable archive) must ship this file and
`LGPL-2.1.txt`.
