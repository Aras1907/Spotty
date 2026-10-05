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

Native Linux x86_64 Cargo builds include the Proton Bridge login window and
verified Bridge runtime. In **Settings → Triggers → Store**, click **Install**
for Proton Mail Bridge. The window starts Bridge and requests your login
information, then shows the generated Bridge password and local IMAP/SMTP
settings to copy into your mail client. No separate Bridge installation is
needed. The trigger starts disabled; Bridge is activated only after opting in.

Use `proton login` or `proton settings` to reopen it. Closing the window keeps
Bridge running. A paid Proton Mail plan and an unlocked Linux keyring are
required. Build with current submodules (`git submodule update --init --recursive`)
and `cargo install --path . --locked`. The build requires Python 3 and network
access once to prepare the pinned native payload. It installs no system packages.
See [package details](trigger-backends/proton-bridge-gui/README.md).
