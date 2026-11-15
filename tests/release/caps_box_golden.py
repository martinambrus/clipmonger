#!/usr/bin/env python3
"""Per-box caps goldens (seg-11 §5 gate 2): record or diff the daemon's
capabilities on one validation box, including the negative cases (Degraded,
Unavailable) as first-class expectations.

  clipmonger-cli --profile-dir P status | caps_box_golden.py BOX            # diff
  clipmonger-cli --profile-dir P status | caps_box_golden.py BOX --bless    # record

Goldens live in tests/golden/boxes/BOX.json. Volatile values (versions,
paths, pids, tokens, timestamps, commands that embed install paths) are
dropped before comparing; levels, reasons and booleans are kept.
"""
import json
import sys
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent.parent / "golden" / "boxes"
VOLATILE = {"version", "pid", "socket", "token", "profile", "ts", "uptime_ms",
            "popup_hotkey_command", "autostart_command", "privacy_skips",
            "clips", "monitoring_paused", "last_capture_ms"}


def normalize(value):
    if isinstance(value, dict):
        return {k: normalize(v) for k, v in sorted(value.items()) if k not in VOLATILE}
    if isinstance(value, list):
        return [normalize(v) for v in value]
    return value


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    box = sys.argv[1]
    text = sys.stdin.read()
    status = json.loads(text[text.find("{"):])
    caps = normalize(status.get("caps", status))
    path = GOLDEN / f"{box}.json"
    rendered = json.dumps(caps, indent=2, ensure_ascii=False) + "\n"
    if "--bless" in sys.argv:
        GOLDEN.mkdir(parents=True, exist_ok=True)
        path.write_text(rendered)
        print(f"recorded {path}")
        return 0
    if not path.exists():
        print(f"no golden for {box}; run with --bless", file=sys.stderr)
        return 2
    if path.read_text() == rendered:
        print(f"{box}: caps match the golden")
        return 0
    import difflib
    sys.stdout.writelines(difflib.unified_diff(path.read_text().splitlines(True), rendered.splitlines(True),
                                               f"golden/{box}", f"actual/{box}"))
    return 1


if __name__ == "__main__":
    sys.exit(main())
