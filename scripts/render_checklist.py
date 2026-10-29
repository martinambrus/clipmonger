#!/usr/bin/env python3
import json, sys, argparse
ap = argparse.ArgumentParser()
ap.add_argument("input", nargs="?", default="-")
ap.add_argument("--limit", type=int, default=5)
args = ap.parse_args()
data = json.load(open(args.input)) if args.input != "-" else json.load(sys.stdin)
# normalize status
def norm(s): return {"pending":"pending","open":"pending","completed":"completed","done":"completed","in_progress":"in_progress","in-progress":"in_progress"}.get(s,s)
total = len(data)
done = sum(1 for r in data if norm(r["status"]) in ("completed",))
prog = sum(1 for r in data if norm(r["status"])=="in_progress")
open_p = total - done - prog
# sort: in_progress first, then pending/blocked/parked/deferred, then completed hidden
order = {"in_progress":0,"pending":1,"blocked":1,"parked":1,"deferred":1,"open":1,"completed":2,"cancelled":3}
visible = sorted([r for r in data if norm(r["status"])!="completed"], key=lambda r: (order.get(norm(r["status"]),9), r["id"]))
hidden_done = done
hidden_pending = max(0, len(visible) - args.limit)
shown = visible[:args.limit]
print(f"{total} tasks ({done} done, {prog} in progress, {open_p} open)")
for r in shown:
    glyph = "◼" if norm(r["status"])=="in_progress" else "◻"
    title = r.get("title") or r.get("subject") or r["id"]
    note = r.get("note") or r.get("reason") or ""
    line = f"  {glyph} {r['id']} — {title}"
    if note: line += f" — {note}"
    # blocked/parked reason
    if norm(r["status"]) in ("blocked","parked","deferred") and r.get("reason"):
        pass
    print(line)
if hidden_pending or hidden_done:
    parts=[]
    if hidden_done: parts.append(f"{hidden_done} completed")
    if hidden_pending: parts.append(f"+{hidden_pending} pending")
    # also need cancelled?
    print(f"   … {', '.join(parts)}")
