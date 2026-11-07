#!/bin/bash
# Unattended Arch install for the clipmonger wlroots test VM.
set -euxo pipefail
D=/dev/vda
sgdisk -Z $D
sgdisk -n1:0:+512M -t1:ef00 -n2:0:0 -t2:8300 $D
mkfs.fat -F32 ${D}1
mkfs.ext4 -F ${D}2
mount ${D}2 /mnt
mount --mkdir ${D}1 /mnt/boot
pacstrap -K /mnt base linux linux-firmware sudo openssh networkmanager vim less \
  sway swaybg swayidle swaylock foot waybar wl-clipboard xorg-xwayland xorg-xauth \
  hyprland kitty xdg-desktop-portal xdg-desktop-portal-wlr xdg-desktop-portal-hyprland xdg-desktop-portal-gtk \
  python python-gobject gtk3 gtk4 ttf-dejavu noto-fonts polkit mesa grim jq xorg-xprop
genfstab -U /mnt >> /mnt/etc/fstab
arch-chroot /mnt /bin/bash -euxo pipefail <<'IN'
ln -sf /usr/share/zoneinfo/Europe/Bratislava /etc/localtime
hwclock --systohc
echo 'en_US.UTF-8 UTF-8' > /etc/locale.gen && locale-gen
echo 'LANG=en_US.UTF-8' > /etc/locale.conf
echo 'KEYMAP=us' > /etc/vconsole.conf
echo archwl > /etc/hostname
echo 'root:@VM_ROOT_PASS@' | chpasswd
useradd -m -G wheel @VM_USER@
echo '@VM_USER@:@VM_PASS@' | chpasswd
echo '@VM_USER@ ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-@VM_USER@ && chmod 440 /etc/sudoers.d/90-@VM_USER@
systemctl enable sshd NetworkManager
bootctl install
printf 'default arch.conf\ntimeout 2\n' > /boot/loader/loader.conf
printf 'title Arch Linux\nlinux /vmlinuz-linux\ninitrd /initramfs-linux.img\noptions root=UUID=%s rw quiet\n' "$(blkid -s UUID -o value /dev/vda2)" > /boot/loader/entries/arch.conf
# Autologin on tty1; ~/.session picks the compositor (sway or hyprland).
mkdir -p /etc/systemd/system/getty@tty1.service.d
printf '[Service]\nExecStart=\nExecStart=-/sbin/agetty -o "-p -f -- \\\\u" --noclear --autologin @VM_USER@ %%I $TERM\n' > /etc/systemd/system/getty@tty1.service.d/autologin.conf
H=/home/@VM_USER@
install -d -o @VM_USER@ -g @VM_USER@ -m 700 $H/.ssh
curl -fsS http://@HOST_IP@:@SERVE_PORT@/key.pub > $H/.ssh/authorized_keys
chmod 600 $H/.ssh/authorized_keys
echo sway > $H/.session
cat > $H/.bash_profile <<'P'
[[ -f ~/.bashrc ]] && . ~/.bashrc
if [[ -z $WAYLAND_DISPLAY && $(tty) == /dev/tty1 ]]; then
  export WLR_RENDERER_ALLOW_SOFTWARE=1 WLR_NO_HARDWARE_CURSORS=1
  case $(cat ~/.session 2>/dev/null) in
    hyprland) export XDG_CURRENT_DESKTOP=Hyprland; exec Hyprland > ~/.session.log 2>&1 ;;
    *) export XDG_CURRENT_DESKTOP=sway; exec sway > ~/.session.log 2>&1 ;;
  esac
fi
P
mkdir -p $H/.config/sway $H/.config/hypr
cp /etc/sway/config $H/.config/sway/config
sed -i 's/^set \$term .*/set $term foot/' $H/.config/sway/config
printf '\noutput * bg #305070 solid_color\noutput Virtual-1 resolution 1280x800\nexec dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP DISPLAY\n' >> $H/.config/sway/config
cat > $H/.config/hypr/hyprland.conf <<'P'
monitor = , 1280x800, auto, 1
exec-once = dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP DISPLAY HYPRLAND_INSTANCE_SIGNATURE
exec-once = waybar
misc { disable_hyprland_logo = true }
ecosystem { no_update_news = true }
$mod = SUPER
bind = $mod, Return, exec, kitty
bind = $mod SHIFT, Q, killactive
bind = $mod SHIFT, E, exit
P
chown -R @VM_USER@:@VM_USER@ $H
IN
echo ARCH-INSTALL-DONE
