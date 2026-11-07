#!/bin/sh
# Compact view of the newest N clips (default 6): id, formats, size, preview.
. ~/clipmonger-test/env.sh 2>/dev/null
PATH=$HOME/.local/bin:$HOME/clipmonger-test/bin:$PATH
clipmonger-cli --profile-dir ~/.local/share/clipmonger list | python3 -c '
import json,sys
n=int(sys.argv[1]) if len(sys.argv)>1 else 6
for c in json.load(sys.stdin)["clips"][:n]:
    keys={k:v for k,v in c.items() if k in ("id","bytes_total","formats")}
    prev=c.get("preview") or c.get("text_preview") or c.get("title") or ""
    print(c.get("id"), c.get("bytes_total"), ",".join(c.get("formats",[]))[:70], repr(str(prev)[:50]))
' "${1:-6}"
