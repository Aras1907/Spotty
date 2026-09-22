// Work out how to uninstall an installed application from its `.desktop` file.
//
// Flatpak apps are removed via `flatpak uninstall`; distro apps are removed via
// the package that owns the `.desktop` file (looked up with rpm/dpkg/pacman)
// run through `pkexec`. Returns a ready-to-run plan for `operations::start`.

use std::path::Path;

#[derive(Clone)]
pub struct Plan {
    pub title: String,
    pub source: String,
    pub icon: String,
    pub args: Vec<String>,
}

fn is_sandbox() -> bool {
    std::env::var("FLATPAK_ID").is_ok()
}

fn host_output(prog: &str, args: &[&str]) -> Option<String> {
    let mut cmd = if is_sandbox() {
        let mut c = std::process::Command::new("flatpak-spawn");
        c.args(["--host", prog]);
        c
    } else {
        std::process::Command::new(prog)
    };
    cmd.args(args);
    let out = cmd.output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

fn host_prefix() -> Vec<String> {
    if is_sandbox() {
        vec!["flatpak-spawn".into(), "--host".into()]
    } else {
        vec![]
    }
}

/// The Flatpak app-id, if this `.desktop` file belongs to a Flatpak app.
fn flatpak_app_id(desktop: &Path) -> Option<String> {
    let s = desktop.to_string_lossy();
    if s.contains("flatpak/exports") || s.contains("/flatpak/app/") {
        return desktop
            .file_stem()
            .map(|x| x.to_string_lossy().to_string())
            .filter(|id| !id.is_empty());
    }
    None
}

pub fn plan_for_app(desktop: &Path, name: &str, icon: &str) -> Option<Plan> {
    // Flatpak first — deterministic from the path.
    if let Some(app_id) = flatpak_app_id(desktop) {
        let mut args = host_prefix();
        args.extend([
            "flatpak".into(),
            "uninstall".into(),
            "--assumeyes".into(),
            "--noninteractive".into(),
            app_id,
        ]);
        return Some(Plan {
            title: format!("Uninstalling {name}"),
            source: "Flatpak".into(),
            icon: icon.to_string(),
            args,
        });
    }

    // Distro: find the package that owns the .desktop file.
    let path = desktop.to_string_lossy().to_string();
    if let Some(out) = host_output("rpm", &["-qf", "--queryformat", "%{NAME}", &path]) {
        let pkg = out.trim();
        if !pkg.is_empty() && !pkg.contains("not owned") {
            return Some(distro_plan("dnf", pkg, name, icon));
        }
    }
    if let Some(out) = host_output("dpkg", &["-S", &path]) {
        if let Some(pkg) = out.split(':').next() {
            let pkg = pkg.trim();
            if !pkg.is_empty() {
                return Some(distro_plan("apt", pkg, name, icon));
            }
        }
    }
    if let Some(out) = host_output("pacman", &["-Qo", &path]) {
        // "<path> is owned by <pkg> <version>"
        if let Some(idx) = out.find("owned by ") {
            if let Some(pkg) = out[idx + 9..].split_whitespace().next() {
                return Some(distro_plan("pacman", pkg, name, icon));
            }
        }
    }
    None
}

/// Whether `bus_name` (e.g. "org.gnome.TextEditor") currently owns a name on
/// the session bus — i.e. a GApplication-based app is running. GTK/libadwaita
/// apps register their application ID as a session-bus name by default, so
/// this works even on pure Wayland (no X11/WM_CLASS needed).
fn dbus_name_has_owner(bus_name: &str) -> bool {
    if !bus_name.contains('.') {
        return false;
    }
    let arg = format!("'{}'", bus_name);
    host_output(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            &arg,
        ],
    )
    .is_some_and(|out| out.contains("true"))
}

/// Candidate process / window-class names for `name` and its `.desktop` file:
/// the display name with spaces stripped or hyphenated, plus the last
/// dot-separated component of the desktop file id (e.g. "org.gnome.Ptyxis"
/// -> "ptyxis") and the full id itself, deduplicated.
fn process_name_candidates(desktop: &Path, name: &str) -> Vec<String> {
    let mut candidates = vec![
        name.to_lowercase().replace(' ', ""),
        name.to_lowercase().replace(' ', "-"),
    ];
    if let Some(stem) = desktop.file_stem().and_then(|s| s.to_str()) {
        let stem_lower = stem.to_lowercase();
        if let Some(last) = stem_lower.rsplit('.').next() {
            let last = last.to_string();
            if !candidates.contains(&last) {
                candidates.push(last);
            }
        }
        if !candidates.contains(&stem_lower) {
            candidates.push(stem_lower);
        }
    }
    candidates
}

/// Whether an instance of `name` (or its Flatpak app-id) currently has a
/// running process / Flatpak instance / session-bus name.
pub fn is_app_running(desktop: &Path, name: &str) -> bool {
    if let Some(app_id) = desktop.file_stem().and_then(|s| s.to_str()) {
        if dbus_name_has_owner(app_id) {
            log::info!("is_app_running: {app_id} has D-Bus session-bus owner");
            return true;
        }
    }
    if let Some(app_id) = flatpak_app_id(desktop) {
        let running = host_output("flatpak", &["ps", "--columns=application"])
            .is_some_and(|out| out.lines().any(|l| l.trim().eq_ignore_ascii_case(&app_id)));
        log::info!("is_app_running: flatpak ps -> {app_id} running = {running}");
        return running;
    }
    // No D-Bus name and not a Flatpak app: fall back to process matching,
    // trying both "no spaces" (e.g. "VS Code" -> "vscode") and "hyphenated"
    // (e.g. "GNOME Terminal" -> "gnome-terminal") forms of the display name,
    // plus forms derived from the .desktop file id itself (e.g.
    // "org.gnome.Ptyxis" -> "ptyxis", since the binary is often named after
    // the last component rather than the display name).
    let candidates = process_name_candidates(desktop, name);
    for proc in &candidates {
        if host_output("pgrep", &["-i", proc]).is_some_and(|out| !out.trim().is_empty()) {
            log::info!("is_app_running: pgrep -i {proc} matched a process");
            return true;
        }
    }
    log::info!(
        "is_app_running: no D-Bus/flatpak/process match for '{name}' (tried {candidates:?})"
    );
    false
}

/// Build a plan to kill a running instance of `name` (the app shown in
/// search). Flatpak apps are killed by app-id via `flatpak kill`; everything
/// else falls back to `pkill -i` against the app's display name.
pub fn kill_plan_for_app(desktop: &Path, name: &str, icon: &str) -> Plan {
    if let Some(app_id) = flatpak_app_id(desktop) {
        let mut args = host_prefix();
        args.extend(["flatpak".into(), "kill".into(), app_id]);
        return Plan {
            title: format!("Killing {name}"),
            source: "Flatpak".into(),
            icon: icon.to_string(),
            args,
        };
    }
    let proc = name.to_lowercase().replace(' ', "");
    let mut args = host_prefix();
    args.extend(["pkill".into(), "-i".into(), proc]);
    Plan {
        title: format!("Killing {name}"),
        source: "Process".into(),
        icon: icon.to_string(),
        args,
    }
}

fn distro_plan(pm: &str, pkg: &str, name: &str, icon: &str) -> Plan {
    let inner: Vec<String> = match pm {
        "dnf" => vec!["dnf".into(), "remove".into(), "-y".into(), pkg.into()],
        "apt" => vec!["apt-get".into(), "remove".into(), "-y".into(), pkg.into()],
        "pacman" => vec![
            "pacman".into(),
            "-R".into(),
            "--noconfirm".into(),
            pkg.into(),
        ],
        _ => vec![pm.into()],
    };
    let mut args = host_prefix();
    args.push("pkexec".into());
    args.extend(inner);
    Plan {
        title: format!("Uninstalling {name}"),
        source: pm.to_string(),
        icon: icon.to_string(),
        args,
    }
}
