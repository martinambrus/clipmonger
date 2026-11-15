#!/usr/bin/env python3
"""T-1103 soak (seg-11 §3, §8): run the real daemon with its X11 monitor for
hours under a steady copy load, then check the perf budgets, the logs and the
database.

Isolation: the daemon gets its own Xvfb display, HOME, XDG dirs, runtime dir
and profile, and no session D-Bus. It never touches the desktop session,
autostart entries or the real profile. Wayland capture is covered by T-604 and
T-605 (a Wayland session ignores X11 captures by design).

Usage:
  tests/soak/soak.py --bin target/linux-release/release --hours 24 --out ~/clipmonger-soak/<date>

Load: a text copy every 3 s (sizes up to 64 KiB), a 400x300 image every 60 s,
a repeat of an older text every 30 s (dedupe path). Retention caps are set low
(300 items) so trimming runs all the time; the hourly retention job runs too.
At the end the copy load stops for 10 minutes to measure idle CPU.

Checks (seg-11 §8 budgets; the report keeps every measured value):
  - daemon idle CPU < 1 %, RSS < 120 MB (max over the run);
  - capture -> stored p95 < 50 ms: the probe copy's time (driver clock)
    against the `created_ms` the daemon stamps when it stores the clip;
  - 10k-clip search budget is covered by the store's perf test, not here;
  - zero error-severity lines in the daemon runtime log;
  - `db.info --check` health ok at the end; daemon still running.
Exit code 0 only when every check passes. report.json + report.md go to --out.
"""
import argparse
import json
import os
import random
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

DRIVER = r'''
import os, random, sys, time, gi
gi.require_version("Gtk", "3.0"); gi.require_version("Gdk", "3.0")
from gi.repository import Gtk, Gdk, GdkPixbuf, GLib
cb = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
rng = random.Random(1103)
texts = []
stop_at = time.time() + float(sys.argv[1])
marker = sys.argv[2]
tick = [0]
def text_copy():
    n = rng.choice([8, 40, 200, 2000, 20000, 65536])
    body = "soak-%d-%d " % (tick[0], time.time_ns()) + "x" * n
    texts.append(body)
    cb.set_text(body, -1)
def image_copy():
    w, h = 400, 300
    px = bytes(rng.getrandbits(8) for _ in range(w * h * 3))
    pb = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(px), GdkPixbuf.Colorspace.RGB, False, 8, w, h, w * 3)
    cb.set_image(pb)
def step():
    if time.time() >= stop_at:
        Gtk.main_quit(); return False
    tick[0] += 1
    if os.path.exists(marker):
        # The harness asks for one probe copy: write it and acknowledge.
        token = open(marker).read().strip()
        os.remove(marker)
        cb.set_text(token, -1)
        with open(marker + ".tmp", "w") as f:
            f.write(str(time.time()))
        os.replace(marker + ".tmp", marker + ".done")  # atomic: never read half-written
    elif tick[0] % 20 == 0:
        image_copy()
    elif tick[0] % 10 == 0 and texts:
        cb.set_text(rng.choice(texts[-200:]), -1)
    else:
        text_copy()
    return True
GLib.timeout_add(3000, step)
Gtk.main()
'''


PROBE_EVERY = 120  # seconds between latency probes


def free_display():
    for n in range(210, 260):
        if not Path(f"/tmp/.X{n}-lock").exists() and not Path(f"/tmp/.X11-unix/X{n}").exists():
            return n
    raise SystemExit("no free X display")


def proc_sample(pid):
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    ticks = int(stat[11]) + int(stat[12])
    rss_kb = 0
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            rss_kb = int(line.split()[1])
    return ticks, rss_kb


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True, type=Path)
    ap.add_argument("--hours", type=float, default=24.0)
    ap.add_argument("--idle-minutes", type=float, default=10.0)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    hz = os.sysconf("SC_CLK_TCK")
    root = Path(tempfile.mkdtemp(prefix="cm-soak-"))
    home, runtime, profile = root / "home", root / "run", root / "profile"
    for d in (home, runtime, profile):
        d.mkdir(mode=0o700)
    (profile / "config.toml").write_text(
        "[history]\nmax_items = 300\nmax_age_days = 1\n[updates]\ncheck = false\n"
    )
    display = free_display()
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(home),
        "XDG_RUNTIME_DIR": str(runtime),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_DATA_HOME": str(home / ".local/share"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "DISPLAY": f":{display}",
        "CLIPMONGER_NO_UI_SPAWN": "1",
        "LANG": "C.UTF-8",
    }
    children = []
    log = open(args.out / "harness.log", "a")

    def say(msg):
        line = f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}"
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()

    def cli(*argv, timeout=30):
        return subprocess.run(
            [str(args.bin / "clipmonger-cli"), "--profile-dir", str(profile), *argv],
            env=env, capture_output=True, text=True, timeout=timeout,
        )

    try:
        xvfb = subprocess.Popen(["Xvfb", f":{display}", "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        children.append(xvfb)
        time.sleep(2)
        daemon = subprocess.Popen([str(args.bin / "clipmonger-daemon"), "--profile-dir", str(profile)],
                                  env=env, stdout=open(args.out / "daemon.out", "w"), stderr=subprocess.STDOUT)
        children.append(daemon)
        for _ in range(100):
            if cli("status").returncode == 0:
                break
            time.sleep(0.2)
        status = json.loads(cli("status").stdout)
        say(f"daemon pid {daemon.pid} up; x11 monitor: {status.get('caps', {}).get('backends', {}).get('x11', {}).get('monitor')}")
        load_secs = args.hours * 3600
        marker = str(root / "probe")
        driver = subprocess.Popen([sys.executable, "-c", DRIVER, str(load_secs), marker],
                                  env={**env, "GDK_BACKEND": "x11"},
                                  stdout=subprocess.DEVNULL, stderr=open(args.out / "driver.err", "w"))
        children.append(driver)
        samples, latencies, rows = [], [], []
        probe_misses = 0
        start = time.time()
        last_ticks, last_t = proc_sample(daemon.pid)[0], time.time()
        next_probe = start + 60
        while time.time() - start < load_secs:
            time.sleep(30)
            if daemon.poll() is not None:
                raise RuntimeError(f"daemon exited with {daemon.returncode}")
            if driver.poll() is not None and time.time() - start < load_secs - 60:
                raise RuntimeError(f"driver exited with {driver.returncode}")
            ticks, rss = proc_sample(daemon.pid)
            now = time.time()
            cpu = (ticks - last_ticks) / hz / (now - last_t) * 100
            last_ticks, last_t = ticks, now
            samples.append({"t": round(now - start), "cpu_pct": round(cpu, 3), "rss_kb": rss})
            if now >= next_probe:
                next_probe = now + PROBE_EVERY
                token = f"probe-{random.getrandbits(48):x}"
                Path(marker).write_text(token)
                done = Path(marker + ".done")
                for _ in range(200):  # the driver picks it up within 3 s
                    if done.exists():
                        break
                    time.sleep(0.02)
                try:
                    copied = float(done.read_text()) if done.exists() else None
                except ValueError:
                    copied = None
                done.unlink(missing_ok=True)
                # The daemon stamps `created_ms` when it stores the clip:
                # stored minus copied is capture -> stored, without the CLI.
                stored = None
                while copied and time.time() - copied < 5 and stored is None:
                    for clip in json.loads(cli("list").stdout or "{}").get("clips", []):
                        if token in (clip.get("preview") or ""):
                            stored = clip.get("created_ms", 0) / 1000
                            break
                    else:
                        time.sleep(0.05)
                if copied and stored:
                    latencies.append(max(0.0, (stored - copied) * 1000))
                else:
                    probe_misses += 1
                info = json.loads(cli("db-info").stdout or "{}")
                rows.append({"t": round(now - start), **info})
                say(f"t={round(now - start)}s cpu={cpu:.2f}% rss={rss / 1024:.1f}MB "
                    f"clips={info.get('clips')} db={info.get('db_bytes')} probe_ms="
                    f"{latencies[-1] if latencies else None} misses={probe_misses}")
        driver.terminate()
        say(f"load done; measuring idle for {args.idle_minutes} min")
        t0, s0 = proc_sample(daemon.pid)[0], time.time()
        time.sleep(args.idle_minutes * 60)
        t1, rss_end = proc_sample(daemon.pid)
        idle_cpu = (t1 - t0) / hz / (time.time() - s0) * 100
        check = json.loads(cli("db-info", "--check", timeout=600).stdout or "{}")
        logs = cli("status")  # still alive?
        log_errors = []
        for path in sorted((profile / "logs").glob("*.log")) if (profile / "logs").exists() else []:
            for line in path.read_text(errors="replace").splitlines():
                if '"sev":"error"' in line:
                    log_errors.append(f"{path.name}: {line[:300]}")
        p95 = statistics.quantiles(latencies, n=20)[18] if len(latencies) >= 20 else (max(latencies) if latencies else None)
        max_rss_mb = max(s["rss_kb"] for s in samples) / 1024 if samples else None
        report = {
            "hours": args.hours, "samples": len(samples), "probes": len(latencies),
            "idle_cpu_pct": round(idle_cpu, 3), "max_rss_mb": round(max_rss_mb, 1) if max_rss_mb else None,
            "end_rss_mb": round(rss_end / 1024, 1),
            "capture_to_stored_p95_ms": round(p95, 1) if p95 else None,
            "probe_misses": probe_misses,
            "health": check.get("health"), "health_detail": check.get("health_detail"),
            "final_db": check, "daemon_alive": logs.returncode == 0,
            "log_errors": log_errors[:50], "log_error_count": len(log_errors),
        }
        checks = {
            "idle_cpu_below_1pct": idle_cpu < 1.0,
            "rss_below_120mb": max_rss_mb is not None and max_rss_mb < 120,
            "capture_to_stored_p95_below_50ms": p95 is not None and p95 < 50,
            "zero_log_errors": not log_errors,
            "db_health_ok": check.get("health") == "ok",
            "daemon_alive": logs.returncode == 0,
            "probes_mostly_found": len(latencies) >= 10 * max(1, probe_misses),
        }
        report["checks"] = checks
        (args.out / "report.json").write_text(json.dumps(report, indent=2))
        (args.out / "samples.json").write_text(json.dumps({"samples": samples, "latencies_ms": latencies, "db": rows}))
        md = ["# T-1103 soak report", "", f"Duration: {args.hours} h load + {args.idle_minutes} min idle", ""]
        md += [f"- {k}: {'PASS' if v else 'FAIL'}" for k, v in checks.items()]
        md += ["", "```json", json.dumps({k: v for k, v in report.items() if k != 'final_db'}, indent=2), "```"]
        (args.out / "report.md").write_text("\n".join(md) + "\n")
        say("checks: " + ", ".join(f"{k}={'PASS' if v else 'FAIL'}" for k, v in checks.items()))
        return 0 if all(checks.values()) else 1
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.send_signal(signal.SIGTERM)
                try:
                    child.wait(10)
                except subprocess.TimeoutExpired:
                    child.kill()
        shutil.copytree(profile / "logs", args.out / "profile-logs", dirs_exist_ok=True) if (profile / "logs").exists() else None
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
