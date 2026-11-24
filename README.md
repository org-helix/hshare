# hShare

Share your screen between computers on the same network, written in Rust with
[egui](https://github.com/emilk/egui), in the Helix look shared with
[netlix](https://github.com/org-helix/netlix).

- **Share my screen**: type the other device's code and your screen appears on it.
- **See their screen**: show the other device's screen here, once its user accepts.
- **Send a file**: it lands in the other device's Downloads folder.
- **USB share button**: plug in the share puck (USB id `022b:3001`) and press it to start or stop sharing.

Each device shows a 6-digit code. The code finds the device on the network and proves
you can see its screen, so nobody can cast to it without being in the room.

## Install

Grab a package from the [releases](https://github.com/org-helix/hshare/releases) page:

| Distro | File | Install |
|---|---|---|
| Debian 12+, Ubuntu 22.04+, Mint | `hshare_*_amd64.deb` | `sudo apt install ./hshare_*_amd64.deb` |
| Fedora, openSUSE | `hshare-*.x86_64.rpm` | `sudo dnf install ./hshare-*.x86_64.rpm` |
| Arch, Manjaro | `hshare-*-x86_64.pkg.tar.zst` | `sudo pacman -U hshare-*-x86_64.pkg.tar.zst` |

Then open **hShare** from the application menu.

On Wayland (KDE, GNOME, sway, Hyprland...) your desktop asks which screen or window to
share each time. On X11 without a portal, hShare shares the whole screen.

## Network

hShare uses TCP port 47800 and UDP port 47801 (discovery by code). If a firewall is on,
open them, for example `sudo ufw allow 47800/tcp && sudo ufw allow 47801/udp`.

When the code isn't found (another subnet, or broadcast blocked), type the device's IP
address before the code: `192.168.1.20 482913`.

Traffic is not encrypted: use hShare on a network you trust.

## Build

```bash
sudo apt install libpipewire-0.3-dev libclang-dev
cargo build --release
```

`cargo build --no-default-features` builds without PipeWire (X11 capture only).

## License

MIT, see [LICENSE](LICENSE). Fonts: Geist and Bricolage Grotesque, SIL Open Font License.
