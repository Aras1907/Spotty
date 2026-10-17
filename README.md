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

## Customization

Open **Settings → Customization** to choose the default Adwaita colors, Nord,
Solarized, Rosé Pine, or your own background, text, and accent colors. Light and
dark modes have separate palettes; Follow System switches with your desktop.
Appearance changes apply immediately and are saved for the next launch.

The same page controls background opacity, window width, visible result count,
text size, result spacing, corner radius, shadow, previews, reduced motion, and
footer indicators. Border controls include visibility, width, solid/dashed/dotted
styles, theme/accent/custom colors, and opacity. Use **Preview Spotty** to open the launcher and **Reset
Appearance** to restore the appearance defaults.

Transparent and Frosted Glass backgrounds keep text and icons opaque. Actual
blur behind the window requires desktop support. On GNOME, enable application
blur in [Blur My Shell](https://github.com/aunetx/blur-my-shell#functionalities)
and add Spotty to its application whitelist. Without desktop blur, Frosted
Glass stays translucent. High contrast uses the desktop's accessible colors
and a solid background.

**Settings → General** includes the launcher shortcut, Start at Login, hiding
when focus moves away, inline completions, and folder path browsing in regular
search. Turning off Start at Login stays disabled after restarting Spotty.
Escape and the launcher shortcut still dismiss the launcher when automatic
hiding is off. Turning off inline completions keeps Tab completion available.

**Settings → Search → Regular Search → Expand Empty Search** expands the
launcher when the regular search box is empty. It lists every enabled,
installed trigger with a word and description, in your configured order.
Select a row or press Enter to open that trigger. The list includes triggers
that only show results in their dedicated mode; scroll to see every word.
Disabled, uninstalled and wordless entries are omitted. This option starts
disabled, and the search box stays empty while you browse the words.

Trigger words are optional for every trigger. Clear **Word** in its settings
(or leave it blank when creating a trigger) to include its results automatically
in regular search. **Show in Regular Search** stays on while the word is empty.
Multiple triggers can have empty words. Disabled triggers still contribute no
results, and shell actions run only when selected.

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
