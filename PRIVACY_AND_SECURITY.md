# Privacy and security

Review date: **2026-10-04**. Applies to Spotty and the `spotty-triggers`
repository, including the backend source compiled through `trigger-backends`.

This describes the current source and the fixes made during a targeted source
review. It is not an independent audit or a guarantee that every vulnerability
has been found. The native Cargo build is the supported verification for this
review; no Flatpak build was used.

## Permissions and trust

Spotty is a desktop launcher running with the permissions of the logged-in
user. It can read indexed files, observe the clipboard while its Clipboard
feature is enabled, launch applications, and perform file operations requested
by the user. Its GNOME extension runs inside GNOME Shell and positions the
launcher window. It is trusted desktop code.

Installing a shell trigger authorizes its command template to run as your user.
It can read, change or delete your files, use the network, or start other
programs. The installation dialog shows the command and defaults to Cancel.
Templates are not sandboxed. Quoting query input does not make an untrusted
template safe: `eval`, interpreters or downstream tools can interpret data as
code. The Run Command feature intentionally executes the command you enter.

Native Store entries enable compiled backends; installing their manifest does
not download Rust code or execute an installer. Backend changes require a new
build. The catalog's `preinstalled` field controls fresh-install defaults only;
the Store uses one searchable list, without a Built-in section. Web manifests
accept only HTTP(S) URLs; files manifests are file-search filters. Installed
manifests are validated again on reload.

The Store trusts the configured `trigger_repo_url`, normally the
[trigger repository](https://github.com/Aras1907/spotty-triggers). Catalogs and
manifests are not independently signed. HTTPS protects transport, but a
compromised repository or maintainer account can still publish harmful shell
templates. Read the installation dialog before granting execution permission.

Distro package operations use PackageKit or dnf5daemon and the system's polkit
authorization. Spotty does not bypass that authorization. Native execution and
host command bridging both grant broad user access; host bridging is not a
security sandbox for triggers.

## Data kept on your machine

The native defaults are `$XDG_CONFIG_HOME/spotty` (usually `~/.config/spotty`)
and `$XDG_CACHE_HOME/spotty` (usually `~/.cache/spotty`). Sandbox builds use
their corresponding XDG directories. These directories are made owner-only
(`0700`) on startup, and protected state writes use files created with `0600`
before data is written. Writes replace the destination atomically. Existing
Spotty directories owned by another user or represented by symlinks are
rejected rather than followed. XDG parent directories are not chmodded.

| Data | Location/content | Retention and control |
|---|---|---|
| Configuration | `config.json`: preferences, shortcuts, pins, endpoint, optional translation API key, and whether Proton Bridge/VPN controls are enabled | Until changed/reset; plaintext, not encrypted; the Proton flag stores no account credentials |
| Clipboard history | `clipboard_history.json`; images under `clipboard-images` in the cache | Captured while Clipboard is enabled and installed, including while the window is hidden. Default limit is 1,000 entries; default time retention is unlimited. Configure retention, remove entries, or disable/uninstall Clipboard to stop new capture |
| Pinned clipboard items | Configuration, including pinned text or image paths | Pins have separate persistence; disabling capture does not erase them |
| Search ranking history | `history.json`: selected queries, result titles and counts | **Off by default**; enable “Keep search history” in General → Privacy. Existing history is retained when switched off; new history is bounded to 1,000 query keys |
| Recent paths and file index | Recent paths, file names, paths and metadata used for search | Stored locally; old records can outlive the original file until refreshed/removed |
| OCR | `ocr.tsv`: extracted document/image text and file paths | Persistent cache; can contain private text even after the original file is removed |
| Previews and thumbnails | Cached document pages, media, images and thumbnails | Persistent caches; these can reveal document contents |
| Commands, translation and operation results | Recent commands, translations and command output in process memory | Normally the daemon lifetime; explicitly enabling search history can also persist selected text/titles |
| IPC | `spotty.pid`, `spotty_keyword.txt`, `spotty_bin` | Local daemon communication; private writes and validated PID identity |

There is **no encryption at rest** in Spotty. File permissions protect against
other ordinary users, but do not protect against root, malware running as your
user, backups, disk access or other software with equivalent access. Clipboard
history can include passwords, tokens and other secrets: Spotty cannot reliably
identify and exclude every secret. Automatically pasting an item uses the
focused application; verify the destination before pasting confidential data.

Turning a feature off stops new collection where specified; it does not erase
its previous state. With Spotty stopped, removing its own history/cache files
clears those records. Removing the entire Spotty configuration directory also
removes preferences, pins and installed triggers. This is not secure erasure
and does not remove copies in backups, logs or the clipboard's current owner.

## Network activity

No analytics, advertising SDK or crash-upload mechanism was found in this
review. This does **not** mean the application never uses the network.

| Feature | Who receives data and when |
|---|---|
| Trigger Store and help images | Opening the Store requests its catalog; installing requests the manifest. Previewing a trigger can download its help image. The configured repository/image host sees the request, IP address and normal connection metadata |
| Translation | Typed text, source/target languages and an optional API key are POSTed to `translate_endpoint` after the typing delay or explicit translation. The default is `http://localhost:5000`; configuring a remote endpoint sends this data to that service over HTTPS. There is no automatic cloud fallback |
| Dictionary backend | If used, dictionary words and suggestion prefixes are sent to DictionaryAPI, Wiktionary and/or Datamuse; following a result may open Dictionary.com |
| Web search and web triggers | Activating the result sends the query to the selected website through your browser. Private browsing does not hide the query or your IP from that website |
| Missing icons | **Downloads are off by default**. Enabling “Download missing icons” contacts search sites or browser-configured icon hosts and Flathub for app icons, revealing domains/app identifiers. Third-party Google and DuckDuckGo favicon fallbacks were removed. Local and already cached icons still work |
| Currency conversion | When used, exchange-rate and currency-name downloads contact jsDelivr or the currency API's Cloudflare Pages endpoint; the rate request does not contain the amount being converted |
| Apps and updates | Configured package managers and repositories can receive install/search/update requests. Automatic update notifications require the Updates provider to be installed/enabled and notifications to be enabled |
| Shell commands/triggers | Their network behavior is determined by the command and the programs it starts |
| Proton VPN controls | When its popup opens or a control is clicked, Spotty invokes the official `protonvpn` CLI on the host. Proton receives the CLI's sign-in, status or connection requests and normal network metadata. Spotty passes only the username for sign-in; password and 2FA stay in Proton's interactive prompt, and VPN account data is not stored by Spotty |

Shared HTTP downloads have time/redirect limits and a hard limit on bytes read.
Remote downloads and translation require HTTPS; HTTP is allowed for the
explicit loopback names `localhost`, `127.0.0.1` and `::1`. HTTPS redirects
cannot downgrade to HTTP or change to non-web protocols. Local HTTP requests
are not automatically redirected. Web browser actions can still open ordinary
HTTP websites; HTTP queries are visible to the network between browser and
server. Environment-configured proxies and external programs may introduce
additional recipients. HTTPS does not prevent the receiving service from
logging the data it receives.

Translation text and API keys are supplied to curl through stdin, rather than
being embedded in process arguments. The key still exists in local configuration
and memory. Ordinary browser launches and explicitly entered shell commands can
include sensitive data in their process arguments. Private-browser launch logs
no longer record the URL; other diagnostic logs can contain paths or operation
output, so inspect logs before sharing them.

## Findings and fixes

Ratings describe potential impact; exploitability depends on an attacker
controlling the relevant manifest, response, file or local state. No working
remote compromise was demonstrated in this review.

| Finding | Impact | Fix/status |
|---|---|---|
| Reload bypassed manifest validation; IDs reached update/uninstall/cache paths | Moderate: malformed local manifests could escape intended paths | Validate IDs at every mutation, reject invalid/duplicate manifests on reload, require file name/ID agreement, reject symlink manifests, cap manifest size |
| Trigger identifiers and executable paths were interpolated into shortcut shell code | Moderate: shell interpretation of metadata | Shortcuts launch the CLI with quoted arguments; shell interpolation removed |
| PID file accepted zero, negative and unrelated process IDs | Moderate: unintended process/group signaling | Require PID > 1, same user and matching executable before sending a signal |
| Private state used default permissions; clipboard used a predictable temporary file | High for confidentiality on shared systems | Owner-only directories and atomic, exclusive-create `0600` state writes; private OCR temporary directories |
| Translation text/API keys were in curl/shell arguments | High for confidential content | JSON body is written through a pipe to stdin; require HTTPS for remote endpoints |
| Unrestricted URL handlers/redirects and unbounded icon responses | Moderate: unexpected handler activation or resource exhaustion | HTTP(S) browser actions, validated download URLs, protocol/redirect restrictions, bounded reads for network helpers/icons/help images |
| Confirmation parsed a manifest again after approval | Moderate: installed command could differ from the displayed command | Install the in-memory manifest snapshot shown in the dialog |
| Quoting `{query}` inside a template could undo query escaping | Moderate: query interpreted as shell syntax | Reject quoted/escaped placeholders and placeholders in substitutions/heredocs; templates remain trusted executable code |
| Text previews read whole files; tar allocated a buffer from the declared size; some ZIP/OLE reads had no bound | High for availability | Bound text reads, stream tar skips with checked arithmetic and a 64 MiB scan budget, cap selected embedded ZIP/OLE reads, enforce the PDF OCR renderer's time budget |
| Copy operations followed directory symlinks and could recurse into their own destination | Moderate: unintended traversal, overwrite or resource exhaustion | Copy symlinks as links, reject copying/moving directories into themselves, exclusive destination creation, atomic no-replace moves |
| Clipboard capture and update checks continued beyond feature removal; search/icon activity lacked explicit privacy controls | Moderate for privacy | Gate clipboard capture and update checks; search history and remote icons are now opt-in |
| Private browser launch logged its full URL | Moderate for privacy | Log the browser identifier without the query/URL |

The storage/network helpers are in
[`trigger-backends/src/security.rs`](trigger-backends/src/security.rs).
Other boundaries are in [`src/triggers.rs`](src/triggers.rs),
[`src/main.rs`](src/main.rs), [`src/app.rs`](src/app.rs),
[`src/config.rs`](src/config.rs), [`src/ui/settings_window.rs`](src/ui/settings_window.rs)
and the backend's `src/features` and `src/search` files.

### Dependencies

The checked-in lockfile was compared with matching entries in the official
[RustSec advisory database](https://github.com/RustSec/advisory-db).
Two affected versions were updated:

- `anyhow` 1.0.102 → 1.0.103:
  [RUSTSEC-2026-0190](https://rustsec.org/advisories/RUSTSEC-2026-0190.html).
- `crossbeam-epoch` 0.9.18 → 0.9.20:
  [RUSTSEC-2026-0204](https://rustsec.org/advisories/RUSTSEC-2026-0204.html).

Their vulnerable functions were not proven reachable from hostile input in
Spotty. Updating avoids retaining known affected code regardless.

Remaining maintenance advisories apply to transitive `paste`, `rustybuzz` and
`ttf-parser` dependencies:
[RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html),
[RUSTSEC-2026-0206](https://rustsec.org/advisories/RUSTSEC-2026-0206.html),
[RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html).
These are maintenance notices, not demonstrated Spotty exploits. Migrating the
GTK/rendering dependency chains needs separate compatibility work. This was a
manual lockfile/advisory comparison, not a complete `cargo audit` run, and does
not cover vulnerabilities in system GTK, curl, poppler, ffmpeg, Tesseract or
bundled C/C++ libraries. Repeat dependency checks before each release.

## Remaining risks and release work

- File previews and OCR still use in-process image, SVG, Office and native
  HEIF/font parsers. External renderer limits and selected allocation limits
  reduce specific denial-of-service paths, but there is no complete parser
  sandbox or uniform CPU/memory budget. Malformed files can still crash or stall
  parsing. Isolate parsers with process resource limits before making stronger
  claims about opening hostile files.
- The application cannot protect against code running with the same user
  permissions. Local clipboard/config modification and executable replacement
  are outside its isolation boundary. PID identity checks narrow signaling
  mistakes but do not provide an authenticated command channel or eliminate all
  process-lifetime races.
- Private state remains plaintext; old backups, previously captured secrets,
  diagnostic logs and caches are not retroactively erased by these changes.
- Store signatures, package provenance and maintainer account security remain
  release responsibilities. Protect repository/release credentials and pin the
  backend submodule when publishing a build.
- The current tracked files were scanned for private-key headers, common GitHub
  token formats and AWS access-key IDs; no matches were found. This narrow check
  does not cover every credential format or Git history. Machine-specific
  `.claude` settings are currently tracked; review/exclude local development
  settings from the release and inspect history before publishing new material.

Verification: native host **`cargo build --locked`**. Compilation alone does
not verify exploit resistance, runtime behavior, parser robustness or every
existing feature. No live user documents were opened or modified during the
review. Vulnerability reporting instructions are in [`SECURITY.md`](SECURITY.md).
