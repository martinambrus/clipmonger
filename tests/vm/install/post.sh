#!/bin/bash
# Run as root on a freshly installed desktop VM: SSH with the host key,
# root password, passwordless sudo for @VM_USER@, autologin, no screen lock.
set -eux
export DEBIAN_FRONTEND=noninteractive
if command -v apt-get >/dev/null; then
  apt-get update -qq && apt-get install -y -qq openssh-server curl python3-gi gir1.2-gtk-3.0 wl-clipboard xclip >/dev/null || true
  systemctl enable --now ssh
fi
echo 'root:@VM_ROOT_PASS@' | chpasswd
echo '@VM_USER@ ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-@VM_USER@ && chmod 440 /etc/sudoers.d/90-@VM_USER@
H=/home/@VM_USER@
install -d -o @VM_USER@ -g @VM_USER@ -m 700 $H/.ssh
curl -fsS http://@HOST_IP@:@SERVE_PORT@/key.pub > $H/.ssh/authorized_keys
chown @VM_USER@:@VM_USER@ $H/.ssh/authorized_keys && chmod 600 $H/.ssh/authorized_keys
# Autologin
if [ -d /etc/lightdm ]; then
  mkdir -p /etc/lightdm/lightdm.conf.d
  printf '[Seat:*]\nautologin-user=@VM_USER@\nautologin-user-timeout=0\n' > /etc/lightdm/lightdm.conf.d/90-autologin.conf
  groupadd -f autologin && usermod -aG autologin @VM_USER@
fi
if [ -f /etc/gdm3/custom.conf ]; then
  sed -i 's/^#\?\s*AutomaticLoginEnable.*/AutomaticLoginEnable=true/; s/^#\?\s*AutomaticLogin\s*=.*/AutomaticLogin=@VM_USER@/' /etc/gdm3/custom.conf
fi
echo POST-DONE
