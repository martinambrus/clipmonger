#!/bin/bash
# linux-suite.sh ALIAS DOMAIN: the core release checks (tests/manual/release-regression.md
# R-02 to R-09) on a desktop whose popup hotkey is Ctrl+Alt+H; prints one line per check.
# SUITE_TERM: terminal launcher taking a command (default 'gnome-terminal --wait --';
# e.g. 'xfce4-terminal -x', 'konsole -e', 'cosmic-term -e', 'foot').
# SUITE_COPY=wl: copy through wl-copy (Sway, Hyprland, COSMIC); the password-hint
# check then needs a focused GTK window and is skipped.
# Uses whichever clipmonger-cli is installed (package, ~/.local/bin, or the dev
# deployment in ~/clipmonger-test/bin).
A=$1; D=$2; V="$(dirname "$0")/vmctl.py"
# Editors open natively on Wayland in wl mode (root cannot use the
# compositor's Xwayland there).
GB=x11; [ "${SUITE_COPY:-x11}" = wl ] && GB=wayland
r() { ssh "$A" ". ~/clipmonger-test/env.sh; export PATH=\$HOME/.local/bin:\$HOME/clipmonger-test/bin:\$PATH; $1" </dev/null; }
if [ "${SUITE_COPY:-x11}" = wl ]; then
    # wlroots compositors pass an Xwayland selection on only from a focused
    # X window; copy through Wayland (data-control) instead.
    r 'cd ~/clipmonger-test; echo -n "suite-text-'$A'" | wl-copy >/dev/null 2>&1; sleep 1.5; python3 mkpng.py 120x80 /tmp/suite.png && wl-copy -t image/png < /tmp/suite.png >/dev/null 2>&1; sleep 1.5'
else
    r 'cd ~/clipmonger-test; (GDK_BACKEND=x11 timeout 8 python3 cmcopy.py text "suite-text-'$A'" >/dev/null 2>&1 &); sleep 2; (GDK_BACKEND=x11 timeout 8 python3 cmcopy.py image 120x80 >/dev/null 2>&1 &); sleep 2'
fi
echo "capture: $(r 'sh ~/clipmonger-test/cmls.sh 2 | cut -d" " -f3- | tr "\n" "|"')"
r 'pkill -f "[e]ditor.py"'; r 'rm -f /tmp/editor.txt*; setsid nohup env GDK_BACKEND='$GB' python3 ~/clipmonger-test/editor.py /tmp/editor.txt >/dev/null 2>&1 < /dev/null & sleep 3'
$V $D click 400 300; $V $D key ctrl+alt+h; sleep 1.5; $V $D type 'suite-text'; sleep 0.8; $V $D key enter; sleep 2.5
echo "popup text -> editor: $(r 'cat /tmp/editor.txt')"
$V $D click 400 300; $V $D key ctrl+alt+h; sleep 1.5; $V $D key down; sleep 0.3; $V $D key enter; sleep 3
echo "popup image -> editor: $(r 'cat /tmp/editor.txt.image 2>/dev/null || echo none')"
r 'clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-list | grep -q PERM- || clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-create Test sig "PERM-'$A'" --hotkey ctrl+alt+k >/dev/null; sleep 1.5'
$V $D click 400 300; $V $D key ctrl+alt+k; sleep 2.5
echo "permanent -> editor: $(r 'cat /tmp/editor.txt')"
r 'pkill -f "[e]ditor.py"'; r 'rm -f /tmp/term.txt; setsid nohup '"${SUITE_TERM:-gnome-terminal --wait --}"' bash -c "stty raw -echo; timeout --foreground 30 cat > /tmp/term.txt" >/dev/null 2>&1 < /dev/null & sleep 4'
$V $D key ctrl+alt+h; sleep 1.5; $V $D type 'suite-text'; sleep 0.8; $V $D key enter; sleep 2.5; $V $D key ctrl+alt+k; sleep 2.5
echo "popup + permanent -> terminal: $(r 'sleep 1; cat /tmp/term.txt')"
r "pkill -f '[c]at > /tmp/term.txt'"
if [ "${SUITE_COPY:-x11}" = wl ]; then
    echo "password hint stored rows: skipped (wl copy mode)"
else
    r '(GDK_BACKEND=x11 timeout 6 python3 ~/clipmonger-test/secret.py "SECRET-'$A'" >/dev/null 2>&1 &); sleep 3'
    echo "password hint stored rows: $(r 'clipmonger-cli --profile-dir ~/.local/share/clipmonger list | grep -c SECRET')"
fi
r 'sudo rm -f /tmp/root-editor.txt*; setsid nohup sudo -E GDK_BACKEND='$GB' python3 ~/clipmonger-test/editor.py >/dev/null 2>&1 < /dev/null & sleep 4'
$V $D click 400 300; $V $D key ctrl+alt+h; sleep 1.5; $V $D type 'suite-text'; sleep 0.8; $V $D key enter; sleep 2.5; $V $D click 400 300; $V $D key ctrl+alt+k; sleep 2.5
echo "popup + permanent -> root editor: $(r 'sudo cat /tmp/root-editor.txt; sudo pkill -f "[e]ditor.py"')"
echo "history: $(r 'sh ~/clipmonger-test/cmls.sh 6 | cut -d" " -f4- | tr "\n" "|"')"
