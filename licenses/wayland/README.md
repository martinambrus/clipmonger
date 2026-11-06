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

Linux release packages (`.deb`, portable archive) must ship this file and
`LGPL-2.1.txt`.
