# Wayland blur protocol bindings (Linux)

The Linux GUI (`clipmonger`) uses winit 0.30 for its windows. winit's Wayland
backend always depends on `wayland-protocols-plasma` 0.3.12, whose manifest
declares MIT. That crate generates Rust bindings at build time from KDE's
`blur.xml` protocol description, and those bindings are compiled into the
Linux binary.

`blur.xml` carries its own license:

    SPDX-FileCopyrightText: 2015 Martin Gräßlin
    SPDX-FileCopyrightText: 2015 Marco Martin
    SPDX-License-Identifier: LGPL-2.1-or-later

Clipmonger treats the generated blur bindings as covered by that license. That
portion of the Linux binary is distributed under the GNU Lesser General Public
License version 2.1 or later; its full text is in `LGPL-2.1.txt`. The rest of
Clipmonger stays under the MIT license in `LICENSE`.

The binary is statically linked. To meet LGPL-2.1 section 6, the complete
corresponding source is public: Clipmonger's own source at its repository, and
`wayland-protocols-plasma` 0.3.12 (with `blur.xml`) on crates.io. You may change
the protocol bindings or the crate and rebuild the binary from that source with
`cargo build --release`, using `[patch.crates-io]` to point at your copy.

Windows builds do not include these bindings.

Linux release packages (`.deb`, portable archive) must ship this file and
`LGPL-2.1.txt`.
