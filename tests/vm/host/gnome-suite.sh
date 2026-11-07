#!/bin/bash
# gnome-suite.sh ALIAS DOMAIN: the GNOME Wayland acceptance journey; prints one line per check.
A=$1; D=$2; V="$(dirname "$0")/vmctl.py"
r() { ssh "$A" ". ~/clipmonger-test/env.sh; $1" </dev/null; }
r 'cd ~/clipmonger-test; (GDK_BACKEND=x11 timeout 8 python3 cmcopy.py text "suite-text-'$A'" >/dev/null 2>&1 &); sleep 2; (GDK_BACKEND=x11 timeout 8 python3 cmcopy.py image 120x80 >/dev/null 2>&1 &); sleep 2'
echo "capture: $(r 'sh ~/clipmonger-test/cmls.sh 2 | cut -d" " -f3- | tr "\n" "|"')"
r 'pkill -f "[e]ditor.py"'; r 'rm -f /tmp/editor.txt*; setsid nohup env GDK_BACKEND=x11 python3 ~/clipmonger-test/editor.py /tmp/editor.txt >/dev/null 2>&1 < /dev/null & sleep 3'
$V $D click 400 300; $V $D key ctrl+alt+h; sleep 1.5; $V $D type 'suite-text'; sleep 0.8; $V $D key enter; sleep 2.5
echo "popup text -> editor: $(r 'cat /tmp/editor.txt')"
$V $D click 400 300; $V $D key ctrl+alt+h; sleep 1.5; $V $D key end; sleep 0.3; $V $D key enter; sleep 3
echo "popup image -> editor: $(r 'cat /tmp/editor.txt.image 2>/dev/null || echo none')"
r '~/clipmonger-test/bin/clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-list | grep -q PERM- || ~/clipmonger-test/bin/clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-create Test sig "PERM-'$A'" --hotkey ctrl+alt+k >/dev/null; sleep 1.5'
$V $D click 400 300; $V $D key ctrl+alt+k; sleep 2.5
echo "permanent -> editor: $(r 'cat /tmp/editor.txt')"
r 'pkill -f "[e]ditor.py"'; r 'rm -f /tmp/term.txt; setsid nohup gnome-terminal --wait -- bash -c "stty raw -echo; timeout --foreground 30 cat > /tmp/term.txt" >/dev/null 2>&1 < /dev/null & sleep 4'
$V $D key ctrl+alt+h; sleep 1.5; $V $D type 'suite-text'; sleep 0.8; $V $D key enter; sleep 2.5; $V $D key ctrl+alt+k; sleep 2.5
echo "popup + permanent -> terminal: $(r 'sleep 1; cat /tmp/term.txt')"
r 'pkill -f "[g]nome-terminal"; (GDK_BACKEND=x11 timeout 6 python3 ~/clipmonger-test/secret.py "SECRET-'$A'" >/dev/null 2>&1 &); sleep 3'
echo "password hint stored rows: $(r '~/clipmonger-test/bin/clipmonger-cli --profile-dir ~/.local/share/clipmonger list | grep -c SECRET')"
r 'sudo rm -f /tmp/root-editor.txt*; setsid nohup sudo -E GDK_BACKEND=x11 python3 ~/clipmonger-test/editor.py >/dev/null 2>&1 < /dev/null & sleep 4'
$V $D click 400 300; $V $D key ctrl+alt+h; sleep 1.5; $V $D type 'suite-text'; sleep 0.8; $V $D key enter; sleep 2.5; $V $D click 400 300; $V $D key ctrl+alt+k; sleep 2.5
echo "popup + permanent -> root editor: $(r 'sudo cat /tmp/root-editor.txt; sudo pkill -f "[e]ditor.py"')"
echo "history: $(r 'sh ~/clipmonger-test/cmls.sh 6 | cut -d" " -f4- | tr "\n" "|"')"
