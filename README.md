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
