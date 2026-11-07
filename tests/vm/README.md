# VM test harness

Tools for the [release regression run](../manual/release-regression.md): they
install test desktops in libvirt/KVM VMs and drive Clipmonger in them from the
host (keys, pointer and screenshots through libvirt, commands over SSH).
Everything here is for test machines only.

## Layout

| Path | What it is |
| --- | --- |
| `host/vmctl.py DOM key\|type\|click\|move\|shot\|size` | Keys (US layout, paced typing), absolute pointer clicks, PNG screenshots of a libvirt domain. Needs the `libvirt` and `PIL` Python modules. |
| `host/build-linux.sh REPO` | Release build in the Debian 12 container, behind a host-wide lock (`CLIPMONGER_BUILD_LOCK`, default `~/.cache/clipmonger-build.lock`) so only one heavy build runs at a time. |
| `host/deploy-linux.sh SSH_ALIAS REPO` | Copies the three binaries, the GNOME extension `dist/` and `guest/linux/` to `~/clipmonger-test` on the VM. |
| `host/gnome-suite.sh SSH_ALIAS DOMAIN` | The GNOME Wayland journey (capture, popup text and image paste, permanent hotkey, terminal, password hint, root window); prints one line per check. |
| `host/deploy-win.sh`, `host/win-*.sh` | Windows: serve the build over HTTP to the VM, run programs in the logged-in desktop session, put files, copy and read the clipboard. |
| `guest/linux/` | `cmcopy.py` (text, image, noise, file copies), `secret.py` (password-hinted copy), `readclip.py`, `editor.py` (GTK3 paste target that saves to a file), `mkpng.py`, `cmls.sh` (compact history), `mkenv.sh PROCESS` (writes `env.sh` with the session's display variables). |
| `guest/windows/` | PowerShell equivalents (`cmcopy.ps1`, `editor.ps1`, `readclip.ps1`, ...). |
| `install/` | Unattended-install templates and `render.sh`. |

## Credentials

The templates hold placeholders only. `install/render.sh OUTDIR` fills them
from `VM_USER`, `VM_PASS`, `VM_ROOT_PASS` (and optional `VM_SSH_KEY`,
`VM_HOSTNAME`, `HOST_IP`, `SERVE_PORT`, `APT_MIRROR`, `APT_SECURITY`) into a
directory outside the repository. Serve that directory to the VMs with
`python3 -m http.server 8770 --bind 192.168.122.1` while installing.

## Installing a test desktop

All VMs: q35, UEFI, 2 vCPUs, 4 GB, 30 GB virtio disk, virtio GPU and NIC,
USB tablet, SPICE. Each ends with SSH by key, passwordless sudo, autologin and
no screen lock.

- **Ubuntu desktop 24.04+ and flavours (Xubuntu):** boot the ISO, press `e` in
  GRUB, append `autoinstall ds=nocloud-net\;s=http://192.168.122.1:8770/<dir>/`
  (rendered `ubuntu-desktop-autoinstall.yaml` saved as `<dir>/user-data`, plus
  `meta-data`). Ubuntu starts the installer by itself; Xubuntu needs its
  "Install" icon started once, then installs unattended.
- **Ubuntu releases without a desktop ISO at hand, or to save download:** the
  official cloud image plus `ubuntu-desktop-minimal` without recommends
  (`ubuntu-cloud-desktop.yaml`, about a quarter of a desktop ISO). Attach your
  own NoCloud seed ISO (`xorriso -as mkisofs -volid CIDATA -joliet -rock
  user-data meta-data`); virt-install's `--cloud-init` seed was ignored. The
  minimal desktop installs a netplan default that hands networking to
  NetworkManager without installing it: divert
  `/usr/lib/netplan/00-network-manager-all.yaml` and `netplan apply`.
- **Linux Mint:** in GRUB add `automatic-ubiquity url=http://.../mint.seed`
  before the trailing `--`; afterwards run `post.sh` from a terminal
  (`wget -qO- http://192.168.122.1:8770/post.sh | sudo bash`).
- **Arch (Sway, Hyprland):** in the live console run
  `curl -fsS 192.168.122.1:8770/k.sh | sh`, then over SSH `bash arch.sh`.
  Hyprland 0.57+ uses a Lua config; startup commands go inside
  `hl.on("hyprland.start", ...)`.
- **Pop!_OS (COSMIC), others with GUI-only installers:** drive the installer
  with `vmctl.py`, then run `post.sh`. For autologin add an `initial_session`
  to `/etc/greetd/cosmic-greeter.toml`.

## Pitfalls

- Run heavy cargo steps (full test builds) with at most one VM up on a 16 GB
  host; two VMs plus a full test build took the desktop session down.
- `pkill -f PATTERN` matches its own SSH command line when the same line also
  contains the pattern (for example, it kills itself while starting
  `editor.py`). Kill in a separate SSH call, or use `pkill -x NAME`.
- Processes started over SSH need `setsid nohup ... &`, or they die with the
  session.
- Raw-mode terminal checks need `timeout --foreground`, or `cat` cannot read
  the TTY.
- Over SSH, run `wl-copy ... >/dev/null 2>&1` and give ssh `</dev/null`, or it
  keeps the session open.
- GRUB drops characters when typed too fast; `vmctl.py type` paces at 80 ms
  (`PACE` overrides).
