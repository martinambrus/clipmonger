#!/usr/bin/env bash
# Release gate runner (seg-11 §5, Step 7). Bounded: every step is
# `timeout`-wrapped. Re-runnable: no state beyond build caches and the
# gnome-ext dist rebuild. Exit code = number of failed AUTO steps.
# SKIP-MANUAL lines are expected for user/native/Ubuntu legs.
set -u
cd "$(dirname "$0")/.." || exit 99

FAILURES=0
declare -a GATE_LINES=()

note() { printf '%s\n' "$*"; }
# run_step <label> <timeout-secs> <cmd...>
run_step() {
    local label="$1" limit="$2"
    shift 2
    note "== $label (timeout ${limit}s): $*"
    if timeout "$limit" "$@"; then
        note "PASS $label"
        return 0
    else
        local rc=$?
        note "FAIL $label (exit $rc)"
        FAILURES=$((FAILURES + 1))
        return 1
    fi
}
gate() { GATE_LINES+=("$1"); }

FMT_OK=0; CLIPPY_OK=0; TESTS_OK=0; X11LIB_OK=0; XVFB_OK=0; WIN_OK=0; WIN_CLIPPY_OK=0; GNOME_OK=0; CAPS_OK=0; DENY_STATE="SKIP"

run_step "fmt" 60 cargo fmt --all --check && FMT_OK=1
run_step "clippy" 600 cargo clippy --workspace --all-targets --all-features -- -D warnings && CLIPPY_OK=1
# Workspace tests excluding the X11 package: the Xvfb suite's harness
# contract is `--test-threads=1` (parallel threads race selection
# ownership despite its serial lock), so it runs only in the explicit
# serial step below. The X11 lib unit tests run in that same package run.
run_step "workspace-tests" 900 cargo test -p clipmonger-core -p clipmonger-platform -p clipmonger-ipc -p clipmonger-daemon -p clipmonger-app-cli -p clipmonger-app-ui -p clipmonger-backend-windows -p clipmonger-backend-wayland && TESTS_OK=1
run_step "x11-lib-tests" 300 cargo test -p clipmonger-backend-x11 --lib && X11LIB_OK=1
run_step "x11-xvfb-serial" 600 cargo test -p clipmonger-backend-x11 --test xvfb_acceptance -- --test-threads=1 && XVFB_OK=1
run_step "windows-gnu-check" 600 cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu && WIN_OK=1
run_step "windows-gnu-clippy" 600 cargo clippy --workspace --all-targets --all-features --target x86_64-pc-windows-gnu -- -D warnings && WIN_CLIPPY_OK=1
if run_step "gnome-ext-build" 300 bash -c 'cd gnome-ext && npm run build'; then
    run_step "gnome-ext-test" 300 bash -c 'cd gnome-ext && npm test' && GNOME_OK=1
fi
run_step "caps-golden" 120 cargo test -p clipmonger-daemon --test caps_golden && CAPS_OK=1

if command -v cargo-deny >/dev/null 2>&1; then
    if run_step "cargo-deny" 300 cargo deny check; then DENY_STATE="PASS"; else DENY_STATE="FAIL"; fi
else
    note "FAIL cargo-deny: required for licenses, advisories, bans and sources (see deny.toml)"
    FAILURES=$((FAILURES + 1))
    DENY_STATE="FAIL"
    if command -v cargo-audit >/dev/null 2>&1; then
        run_step "cargo-audit-advisories-only" 300 cargo audit || :
    fi
fi

# --- seg-11 §5 gate table -------------------------------------------------
if [ "$FMT_OK" = 1 ] && [ "$CLIPPY_OK" = 1 ] && [ "$TESTS_OK" = 1 ] && [ "$X11LIB_OK" = 1 ] && [ "$XVFB_OK" = 1 ] && [ "$WIN_OK" = 1 ] && [ "$WIN_CLIPPY_OK" = 1 ] && [ "$GNOME_OK" = 1 ]; then
    gate "GATE 1 full-pyramid-matrix: PASS (here legs) "
else
    gate "GATE 1 full-pyramid-matrix: FAIL (see step FAILs above)"
fi
gate "SKIP-MANUAL gate1-windows-session-suite: session_acceptance needs a native Windows session"
gate "SKIP-MANUAL gate1-gnome-legs: GNOME Shell legs blocked on Ubuntu GNOME box"
if [ "$CAPS_OK" = 1 ]; then
    gate "GATE 2 caps-golden: PASS (headless golden)"
else
    gate "GATE 2 caps-golden: FAIL"
fi
gate "SKIP-MANUAL gate2-per-box-goldens: Win/GNOME validation-box goldens not capturable here"
gate "SKIP-MANUAL gate3 manual-qa-signed: scripts in tests/manual/ await user execution + signoff"
gate "SKIP-MANUAL gate4 perf+coverage: perf snapshot in evidence/perf.txt (calibration); coverage floor deferred per seg-11 §9"
if [ -f CHANGELOG.md ] && [ -f docs/release-notes-0.1.0.md ]; then
    gate "GATE 5 changelog+upgrade-notes: PASS (drafted; signoff record is user-filed)"
else
    gate "GATE 5 changelog+upgrade-notes: FAIL (files missing)"
    FAILURES=$((FAILURES + 1))
fi
gate "SKIP-MANUAL gate5-signoff-record: two-signature signoff is a user action"
case "$DENY_STATE" in
    PASS) gate "GATE 6 dependency-review: PASS" ;;
    FAIL) gate "GATE 6 dependency-review: FAIL" ;;
    *)    gate "GATE 6 dependency-review: FAIL (full dependency review unavailable)" ;;
esac
gate "SKIP-MANUAL gate7 traceability-sweep: T-315..T-318 added; full F->T sweep + issue filing is user-approved"

note ""
note "--- seg-11 §5 gate table ---"
for line in "${GATE_LINES[@]}"; do note "$line"; done
note "FAILURES=$FAILURES"
if [ "$FAILURES" -gt 0 ]; then exit "$FAILURES"; fi
exit 0
