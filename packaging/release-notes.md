## Install

Every package comes for **x86_64** (Intel/AMD PCs) and **aarch64** (64-bit ARM, e.g. Raspberry Pi 5, Asahi Linux Macs, Snapdragon laptops).
Not sure which one you have? Run `uname -m`.

| Package | x86_64 | ARM (aarch64) | For |
|---|---|---|---|
| Flatpak | `Spotty-@VER@-x86_64.flatpak` | `Spotty-@VER@-aarch64.flatpak` | Any distro with Flatpak (recommended) |
| Debian / Ubuntu | `spotty_@VER@_amd64.deb` | `spotty_@VER@_arm64.deb` | Debian 13+, Ubuntu 25.04+ and derivatives |
| Fedora | `spotty-@VER@-1.fc44.x86_64.rpm` | `spotty-@VER@-1.fc44.aarch64.rpm` | Fedora 44+ |
| Arch Linux | `spotty-@VER@-1-x86_64.pkg.tar.zst` | `spotty-@VER@-1-aarch64.pkg.tar.zst` | Arch Linux, Arch Linux ARM, Manjaro, EndeavourOS |
| Snap | `spotty_@VER@_amd64.snap` | `spotty_@VER@_arm64.snap` | Any distro with snapd |
| AppImage | `Spotty-@VER@-x86_64.AppImage` | `Spotty-@VER@-aarch64.AppImage` | Any recent distro, no install |

Click **Spotty** in your app grid to open its settings. After that, summon the
launcher with its shortcut (Super+Space by default).

### Flatpak

Open the `.flatpak` file. GNOME Software (or KDE Discover) opens, so click **Install**.
If the GNOME runtime is missing, it is downloaded from Flathub automatically.

```sh
flatpak install --user Spotty-@VER@-x86_64.flatpak   # or the aarch64 file on ARM
flatpak run com.spotty.Spotty --settings
```

### Debian / Ubuntu

```sh
sudo apt install ./spotty_@VER@_amd64.deb   # or spotty_@VER@_arm64.deb
```

Log out and back in once so image OCR is picked up.

### Fedora

```sh
sudo dnf install ./spotty-@VER@-1.fc44.x86_64.rpm   # or the aarch64 file
```

### Arch Linux

```sh
sudo pacman -U spotty-@VER@-1-x86_64.pkg.tar.zst   # or the aarch64 file
```

For text search inside images, also install `tesseract` and `tesseract-data-eng`.

### Snap

The snap is not in the Snap Store, and it needs classic confinement
(Spotty launches your apps and manages GNOME shortcuts):

```sh
sudo snap install --dangerous --classic spotty_@VER@_amd64.snap   # or the arm64 file
```

### AppImage

```sh
chmod +x Spotty-@VER@-x86_64.AppImage
./Spotty-@VER@-x86_64.AppImage
```

Shortcuts and **Start at login** point at the AppImage file, so keep it where it is
once you've set them up.

Checksums are in `SHA256SUMS`.

## What's new

- **Clicking Spotty in the app grid opens Settings.** Before, it started Spotty
  hidden in the background, so nothing seemed to happen.
- **Fixed the Flatpak (x86_64 and ARM) doing nothing when launched.** Each
  `flatpak run` has its own process IDs, so a second launch could mistake itself
  for the running Spotty and quit. Flatpak and AppImage launches now reach the
  running Spotty over D-Bus.
- **AppImage:** shortcuts and Start at login now work after the AppImage closes.
- **New packages:** Arch Linux and snap, plus ARM (aarch64) builds of every package.
- Translations now load in the `.deb`, `.rpm` and AppImage without logging out first.
