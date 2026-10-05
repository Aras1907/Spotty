# Spotty

A Raycast-style launcher for GNOME Linux.

## Build from source

The trigger and result implementations live in
[spotty-triggers](https://github.com/Aras1907/spotty-triggers), pinned through
the `trigger-backends` Git submodule. This repository contains Spotty's
application lifecycle, shared UI, configuration, indexer, and platform integration.
Native feature services and their settings dialogs live in the submodule.

```sh
git clone --recurse-submodules https://github.com/Aras1907/Spotty.git
cd Spotty
cargo build
cargo run -- --daemon
```

For an existing checkout, run `git submodule update --init --recursive`
after pulling. Cargo compiles the pinned trigger source into Spotty; it
does not fetch or execute Store code at runtime. Change trigger logic in
the submodule, push it to `spotty-triggers`, and commit the new submodule
revision here.

## Privacy and security

Spotty runs with your user's desktop and file permissions. Shell triggers run
commands as your user and require confirmation before installation. Translation
uses a local endpoint by default; a configured remote endpoint receives your
text. Clipboard, OCR and preview caches can contain confidential data.

Search history and remote icon downloads are opt-in. Private state is stored in
owner-only directories, with protected atomic writes. Previews still parse
untrusted files without a complete process sandbox.

Read [PRIVACY_AND_SECURITY.md](PRIVACY_AND_SECURITY.md) for data storage,
network recipients, patched findings and remaining risks. Report vulnerabilities
using [SECURITY.md](SECURITY.md).

The manifests and backend source are documented in
[spotty-triggers](https://github.com/Aras1907/spotty-triggers).

### Optional Proton Mail Bridge

Linux x86_64 builds include the verified Proton Bridge runtime and its login
controls in Spotty. In **Settings → Search → Store**, click **Install** for
Proton Mail Bridge. Its installed service row then appears under
**Server-side installations**. Click **Settings** to sign in, manage accounts,
and copy the generated Bridge password and local IMAP/SMTP settings into your
mail client, all within Spotty's Settings window.

Bridge is an optional service, without a search trigger or separate application
installation. Uninstall removes its Settings row and account page; it remains
available to reinstall from the Store. Closing its settings keeps the mail
service running. A paid Proton Mail plan and an unlocked Linux keyring are
required.

Build with current submodules (`git submodule update --init --recursive`) and
`cargo install --path . --locked`. Restart a running Spotty after installing a
new binary so its cached settings use the updated interface. The build requires
Python 3 and network access once to prepare the pinned native payload. It
installs no system packages. The Flatpak manifest uses the same embedded Bridge
controls and enables network and Secret Service access for the bundled backend.
See [package details](trigger-backends/proton-bridge-gui/README.md).
