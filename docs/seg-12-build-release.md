# Build + release ($0 pipeline per D-018)

## 1. Repo + toolchain

- Cargo workspace (seg-02 §2) + `gnome-ext/` + `tests/` + `scripts/`
  (release automation = shell + cargo + pinned JS toolchain, no custom
  release framework — review finding 25). Pin BOTH toolchains: Rust
  (rust-toolchain.toml, §1 above) AND the GNOME extension build
  (Node/Deno/Bun choice + TypeScript + ts-for-gir flavor — decide at
  implementation, record here). Enumerate packaging host tools per OS
  (cargo-packager + any native requirements: WiX/NSIS bits on Windows,
  Xcode CLT on macOS, deb/rpm/AppImage tooling on Linux).
- Stable Rust; `rust-toolchain.toml` pins the EXACT version per release
  (bumped deliberately by agents); `Cargo.lock` committed (binary outputs).
  MSRV = latest stable at each release (seg-02); `rust-version` set in
  every crate manifest. fmt+clippy gate PRs (seg-11 §9). Version skew
  across machines is a non-issue: `rust-toolchain.toml` + lockfile pin
  everything (review L-03).

## 2. Build matrix (NATIVE runners per OS — no cross-compile in v1)

v1: win-x64 × linux-x64 (+linux-arm64 in v3); mac-arm64 × mac-x64
from v2 (finding 2). Windows ARM64 explicitly OUT until v3+
(review M-10). All × {full, minimal}. Linux builds run on the
declared oldest-tested LTS image so
artifacts require ≤ the declared oldest-tested LTS glibc (D-012 as
amended, finding 24 — glibc 2.17 is the RUST floor, not our app promise;
Ubuntu 24.04 verified shipping 2.39). CI asserts the floor (exact check
tooling = VERIFY at implementation).
Linux→Windows cross = optional later (verify toolchain if pursued);
Linux→macOS cross explicitly OUT (SDK pain).

## 3. Packaging (cargo-packager 0.11, Apache-2.0 OR MIT, verified)

One bundler CLI for all artifacts (Tauri-bundler lineage documented
.app/.dmg/.deb/.rpm/.AppImage/.exe-NSIS/.msi-WiX; exact format list =
VERIFY at implementation):

- Windows: NSIS installer + portable `.zip` (F-504).
- macOS: `.dmg` + `.app` (LSUIElement bundle, seg-08 §5); per-arch
  artifacts from v2 (finding 2), universal binary evaluated in v2.
- Linux: `.deb` + `.rpm` + `.AppImage` + portable tarball.
- GNOME extension `.zip` (seg-07 installer doubles as packager input).
- Icons/assets per OS; bundle IDs reserved under the FINAL project name.

## 4. Release flow (GitHub Releases, $0, verified docs)

Tag `vX.Y.Z` → matrix CI → packages → Release with auto-generated notes
(curated by agents) + `SHA256SUMS` for every artifact (quotas verified:
1000 assets/release, <2 GiB/file, no total/bandwidth cap).
Update check (NOT auto-install, D-018): client polls the releases feed,
compares semver, notifies + opens the browser. No download/execute.
Default ON (review M-10/L-02: security updates must reach users who
never open Settings); first-run notice + one-tap off + offline-safe
silent skip.

## 5. Unsigned consequences (D-018 — document, don't pay... yet)

- Windows SmartScreen + macOS Gatekeeper WILL friction first installs.
  Ship per-OS bypass docs (verified-accurate on CURRENT OSs at each
  release — T-1204) instead of certificates. Demand signal: issues
  tagged `packaging-trust`; revisit paid signing when real users complain.
- Linux: no signing friction expected.

## 6. Versioning + changelog

Semver; `v1.0.0` = v1 scope per seg-01 §9. `CHANGELOG.md` with
Added/Changed/Fixed per release. MSRV bumps ride minor versions + note.

## 7. Flathub track (v2+, evaluate — do NOT assume)

Flatpak distributes sandboxed desktop apps cross-distro (docs verified)
— and SANDBOXED is the headline caveat: a clipboard manager needs broad
system access. v2 task: prove a permission set that keeps FULL function
(talk-name/filesystem punches? portals?) or stay unpacked; decide with
evidence, never on hope.

## 8. Supply chain (light — review H-02: checksums ≠ signatures)

Committed lockfile + published checksums + dependency audit in CI
(VERIFY tool at implementation). Trust scope (review finding 42):
`SHA256SUMS` = INTEGRITY ONLY (same-account compromise defeats it); $0
provenance options (maintainer-signed git tags with published key, CI
build attestations where available) are evaluated, never conflated with
OS code-signing trust. No other claims in v1.

## 9. Segment tests (T-12xx)

- T-1201 `[e2e]` install-from-ARTIFACT smoke per OS per format (not just
  from source).
- T-1202 `[integration]` update-check vs mock feed (newer/same/older/
  unreachable).
- T-1205 `[e2e]` release-flow dry run on a schedule (tag→matrix→
  packages→notes→SHA256SUMS on a scratch tag), proving the pipeline
  without shipping (review M-04).
- T-1203 `[integration]` glibc-floor assertion on linux artifacts.
- T-1204 `[manual-qa]` bypass-docs accuracy on clean VMs (Win + macOS).
  QA-LEAD signoff.

## 10. Verification tasks

1. cargo-packager exact format list + config shape.
2. macOS universal-binary decision (v2 at latest).
3. glibc-floor check tooling.
4. Dependency-audit tool.
5. Bypass-docs accuracy process (per release).
6. Flathub permission proof (v2 track).
