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

Click **Spotty** in your app grid to open its Settings. Spotty keeps running
in the background after you close them: summon the launcher with
**Ctrl+Space** (or **Alt+Space** if another app already uses Ctrl+Space).

### Flatpak

Open the `.flatpak` file. GNOME Software (or KDE Discover) opens, so click **Install**.
If the GNOME runtime is missing, it is downloaded from Flathub automatically.

```sh
flatpak install --user Spotty-@VER@-x86_64.flatpak   # or the aarch64 file on ARM
flatpak run com.spotty.Spotty --settings   # opens Settings; Spotty stays in the background
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

- **Search finds what you typed.** An app whose name starts with your query
  now comes first, closest match on top: typing "librewolf" offers to install
  LibreWolf before LibreOffice apps that only looked similar.
- **Less memory.** Large decoded images and OCR buffers go straight back to
  the system when Spotty is done with them, and memory freed by background
  work (OCR, indexing) is returned once a minute while Spotty is hidden.
- **Instant shortcut in the Flatpak.** Ctrl+Space no longer starts a new
  Flatpak sandbox on every press: it tells the running Spotty over D-Bus,
  about 10 ms instead of a third of a second. Spotty re-registers the
  shortcut on its next start.
- **Security hardening for hostile files.** Previews and content search now
  refuse oversized or zip-bomb Office files, CSVs and HEIC/AVIF images
  instead of reading them whole, and OCR skips images over 64 MiB. The
  spreadsheet parser (calamine) is updated to 0.38, fixing a crash and
  memory exhaustion on malformed `.xls` files (RUSTSEC-2026-0436).
- **Security:** snap and Flatpak names taken from `.desktop` files can no
  longer be read as command-line options when uninstalling, notifications
  can't be turned into `notify-send` options, and the currency cache is
  written owner-only like the rest of Spotty's state.
- **Install, update and uninstall distro packages on any distro with
  PackageKit**, not only Fedora, Debian/Ubuntu, Arch and openSUSE: Alpine and
  postmarketOS (apk), Gentoo, Solus and others now work too. Uninstalling a
  distro app goes through PackageKit instead of a root password prompt.
- **Security:** Bluetooth device addresses are validated before Spotty runs
  `bluetoothctl`, so a crafted device name can no longer inject commands.
- **Keyboard shortcuts now work outside GNOME Shell.** On Plasma (desktop and
  Mobile), Hyprland, postmarketOS and other desktops, Spotty registers its
  shortcuts through the desktop's Global Shortcuts portal; the desktop may
  ask you once to confirm them. GNOME keeps using its custom shortcuts.
- **Clicking the Spotty icon opens Settings again**, and Spotty keeps running
  in the background after you close them. (In 0.6.3 and 0.6.4 a click started
  it hidden, which looked like nothing happened.)
- **Running `spotty` (or `flatpak run com.spotty.Spotty`) while Spotty runs
  opens the launcher**; before, it silently did nothing.
- Removed two harmless warnings printed by `flatpak run`.
- **New default shortcut: Ctrl+Space.** If another app already uses
  Ctrl+Space, Spotty takes Alt+Space instead (and removes Alt+Space from
  GNOME's window menu so it works). Existing shortcuts are kept.
- Delete-previous-word in the search box is now Ctrl+Backspace by default.
- Flatpak builds for x86_64 and ARM, also in the Flatpak repository:
  `flatpak install --user https://aras1907.github.io/Spotty/com.spotty.Spotty.flatpakref`
