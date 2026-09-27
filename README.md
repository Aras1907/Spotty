# Spotty

A Raycast-style launcher for GNOME Linux.

## Security & trust

Spotty runs as your user with broad desktop access — file search over
`$HOME`, `flatpak-spawn --host` for gsettings / xdotool / curl, and a
GNOME Shell extension for global shortcuts. That is what a desktop
launcher needs; these are the trust boundaries:

- **Triggers from the Store are untrusted input.**
  - A **shell** trigger can run arbitrary commands as you. Spotty always
    shows the exact command in a confirmation dialog (Cancel is the
    default) before installing it, and `{query}` is single-quote-escaped,
    so typed input can never inject extra shell commands. Only install
    shell triggers you trust.
  - **Web** and **files** triggers install directly — a web trigger can
    only open a link in your browser, never run code.
- **The Store's trust anchor is `trigger_repo_url`** (default:
  `github.com/Aras1907/spotty-triggers`) — whoever can push there can
  publish triggers to everyone using it. Downloads are size-capped
  (1 MiB), redirect-capped, and fail on HTTP errors; generated files are
  written with `create_new` in the temp dir, so a pre-planted symlink is
  never written through.
- **Manifests are validated** before they enter
  `~/.config/spotty/triggers/`: id charset (no path traversal), trigger-word
  collisions, non-empty action. `help_image` URLs are restricted to
  http(s), size-capped, and cached under the triggers dir.
- **Previews parse untrusted local files in-process** (images, Office,
  PDF; HEIC via the C++ libheif) — a crash while previewing a malformed
  file is the same class of bug as in any file viewer.
- **Translation is local-only.** The `translate` trigger POSTs your text
  exclusively to `translate_endpoint` in the config — LibreTranslate's
  local default (`http://localhost:5000`, bound to 127.0.0.1). No Google,
  no cloud: if the engine isn't running, Spotty says so instead of falling
  back to an online service. Only point the endpoint somewhere else if you
  accept that your text then leaves your machine.
- No telemetry, no accounts: config and caches live under
  `~/.config/spotty` and `~/.cache/spotty`.

The trigger manifests themselves are documented in
[spotty-triggers](https://github.com/Aras1907/spotty-triggers).
