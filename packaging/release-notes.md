## Install

### Flatpak repository (recommended)

```sh
flatpak install --user https://aras1907.github.io/Spotty/com.spotty.Spotty.flatpakref
```

This adds Spotty's repository and installs the right build for your machine
(x86_64 or ARM); updates then come with `flatpak update` or GNOME Software.
See <https://aras1907.github.io/Spotty/>.

### Packages

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

- **Spotty now has its own Flatpak repository**, for x86_64 and ARM. Install once
  and updates come with `flatpak update` or GNOME Software:
  `flatpak install --user https://aras1907.github.io/Spotty/com.spotty.Spotty.flatpakref`
- **Much lower memory use.** Spotty used to start up to four `flatpak` helper
  processes of ~450 MB each to read Flathub's app list and check for updates.
  It now reads Flathub's app list directly (~30 MB) and checks for updates with
  lightweight queries; heavy package-manager checks run one at a time.
- **Previews settings for Find:** switch result thumbnails and each preview type
  (images, PDF, Office, videos, audio, text, archives, 3D models) on or off.
- **3D model previews and thumbnails:** STL, OBJ, PLY, OFF, glTF, 3MF and Blender
  files.
- **Images saved with the wrong extension** (a JPEG named `.png`) now preview.
- **Text search inside images now works in the Flatpak.**
- **LibreTranslate was removed.** Translation uses web services only (Google
  Translate's free service by default, MyMemory, DeepL and others in Settings).
- **Fixed the Flatpak doing nothing when launched** on installs built from the
  main branch.
