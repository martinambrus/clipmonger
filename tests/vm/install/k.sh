#!/bin/sh
# Live-ISO bootstrap: let the host's test key log in as the current user.
mkdir -p ~/.ssh && chmod 700 ~/.ssh
curl -fsS http://@HOST_IP@:@SERVE_PORT@/key.pub >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys
(systemctl start sshd 2>/dev/null || systemctl start ssh 2>/dev/null || true)
ip -4 -br addr | grep -v '^lo'
