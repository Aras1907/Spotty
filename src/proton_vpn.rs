//! Spotty's in-app Proton VPN client. Proton's official Linux client library
//! (embedded by `trigger-backends/proton-vpn-embedded`) runs in a helper
//! process that Spotty talks to over JSON lines; no Proton app or CLI has to
//! be installed. Passwords and 2FA codes go straight to Proton's library over
//! the helper's stdin and are never stored by Spotty. Proton's library keeps
//! the session in the desktop keyring.
//!
//! Every call here blocks: run them off the GTK main thread.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub use crate::search::proton::VpnTarget;

/// What the client last reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub logged_in: bool,
    pub account: String,
    pub plan: String,
    /// Proton's connection state: Disconnected, Connecting, Connected,
    /// Disconnecting or Error.
    pub state: String,
    pub server: String,
    pub country: String,
    pub error: String,
}

impl Status {
    fn merge(&mut self, value: &Value) {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        if let Some(logged_in) = value.get("logged_in").and_then(Value::as_bool) {
            self.logged_in = logged_in;
        }
        for (key, slot) in [
            ("account", &mut self.account),
            ("plan", &mut self.plan),
            ("state", &mut self.state),
            ("server", &mut self.server),
            ("country", &mut self.country),
            ("error", &mut self.error),
        ] {
            if let Some(v) = text(key) {
                *slot = v;
            }
        }
    }

    pub fn connected(&self) -> bool {
        self.state == "Connected"
    }

    pub fn busy(&self) -> bool {
        matches!(self.state.as_str(), "Connecting" | "Disconnecting")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStep {
    Done,
    TwoFactor,
    Failed(String),
}

/// A country Proton's server list offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub code: String,
    pub name: String,
    pub cities: Vec<String>,
    pub servers: u64,
    /// At least one of its servers is on the user's plan.
    pub available: bool,
}

impl Country {
    fn from_value(value: &Value) -> Option<Country> {
        Some(Country {
            code: value.get("code")?.as_str()?.to_owned(),
            name: value.get("name")?.as_str()?.to_owned(),
            cities: value
                .get("cities")
                .and_then(Value::as_array)
                .map(|cities| cities.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
                .unwrap_or_default(),
            servers: value.get("servers").and_then(Value::as_u64).unwrap_or(0),
            available: value.get("available").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// Proton's connection settings (a subset of Proton's own app).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub protocol: String,
    /// (id, label) of the protocols this system can use.
    pub protocols: Vec<(String, String)>,
    /// 0 off, 1 on, 2 permanent.
    pub killswitch: i64,
    /// 0 off, 1 malware, 2 malware, ads and trackers.
    pub netshield: i64,
    pub vpn_accelerator: bool,
    pub moderate_nat: bool,
    pub port_forwarding: bool,
    pub ipv6: bool,
}

impl Settings {
    fn from_value(value: &Value) -> Settings {
        let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
        Settings {
            protocol: value.get("protocol").and_then(Value::as_str).unwrap_or("wireguard").to_owned(),
            protocols: value
                .get("protocols")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|p| {
                            Some((p.get("id")?.as_str()?.to_owned(), p.get("label")?.as_str()?.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            killswitch: value.get("killswitch").and_then(Value::as_i64).unwrap_or(0),
            netshield: value.get("netshield").and_then(Value::as_i64).unwrap_or(0),
            vpn_accelerator: flag("vpn_accelerator"),
            moderate_nat: flag("moderate_nat"),
            port_forwarding: flag("port_forwarding"),
            ipv6: flag("ipv6"),
        }
    }
}

/// True when this Spotty build carries Proton's client.
pub fn available() -> bool {
    spotty_proton_vpn_embedded::bundled()
}

fn cache_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("spotty")
        .join("proton-vpn")
}

// ── Helper process ──────────────────────────────────────────────────────────

type Reply = Result<Value, String>;

struct Helper {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
}

static HELPER: Mutex<Option<Arc<Helper>>> = Mutex::new(None);
static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn update_status(value: &Value) {
    let mut status = STATUS.lock().unwrap();
    status.get_or_insert_with(Status::default).merge(value);
}

/// Ask the UI to redraw anything that shows VPN state.
fn notify_ui() {
    glib::MainContext::default().invoke(crate::app::refresh_search_window);
}

fn start_helper() -> Result<Arc<Helper>, String> {
    let root = cache_root();
    let installed = spotty_proton_vpn_embedded::install(&root)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("helper.log"))
        .map_err(|e| e.to_string())?;
    let mut child = Command::new(&installed.python)
        .arg("-I")
        .arg(&installed.helper)
        .arg(&installed.bundle)
        .arg(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .map_err(|e| {
            format!(
                "Couldn't start the Proton VPN client ({}: {e}). It needs {} with PyGObject and NetworkManager's GObject bindings.",
                installed.python, installed.python
            )
        })?;
    let stdin = child.stdin.take().ok_or("No stdin for the Proton VPN client")?;
    let stdout = child.stdout.take().ok_or("No stdout for the Proton VPN client")?;
    let helper = Arc::new(Helper {
        stdin: Mutex::new(stdin),
        child: Mutex::new(child),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        alive: AtomicBool::new(true),
    });
    let reader = helper.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            dispatch(&reader, &message);
        }
        reader.alive.store(false, Ordering::SeqCst);
        let _ = reader.child.lock().unwrap().wait();
        for (_, waiter) in reader.pending.lock().unwrap().drain() {
            let _ = waiter.send(Err("The Proton VPN client stopped. Details are in its log.".into()));
        }
        log::warn!("proton vpn: helper exited");
    });
    Ok(helper)
}

fn dispatch(helper: &Helper, message: &Value) {
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let reply = if message.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(message.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(message.get("error").and_then(Value::as_str).unwrap_or("Proton VPN failed").to_owned())
        };
        if let Some(waiter) = helper.pending.lock().unwrap().remove(&id) {
            let _ = waiter.send(reply);
        }
        return;
    }
    match message.get("event").and_then(Value::as_str) {
        Some("ready") | Some("state") => {
            if let Some(data) = message.get("data") {
                update_status(data);
            }
            notify_ui();
        }
        Some("signed_out") => {
            update_status(&json!({"logged_in": false, "account": "", "plan": ""}));
            notify_ui();
        }
        Some("error") => log::warn!("proton vpn: {}", message.get("data").unwrap_or(&Value::Null)),
        _ => {}
    }
}

fn helper() -> Result<Arc<Helper>, String> {
    let mut slot = HELPER.lock().unwrap();
    if let Some(helper) = slot.as_ref().filter(|h| h.alive.load(Ordering::SeqCst)) {
        return Ok(helper.clone());
    }
    let helper = start_helper()?;
    *slot = Some(helper.clone());
    Ok(helper)
}

fn request(cmd: &str, args: Value, timeout: Duration) -> Result<Value, String> {
    let helper = helper()?;
    let id = helper.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    helper.pending.lock().unwrap().insert(id, tx);
    let line = json!({"id": id, "cmd": cmd, "args": args}).to_string();
    let written = {
        let mut stdin = helper.stdin.lock().unwrap();
        writeln!(stdin, "{line}").and_then(|_| stdin.flush())
    };
    if let Err(e) = written {
        helper.pending.lock().unwrap().remove(&id);
        return Err(format!("The Proton VPN client isn't responding: {e}"));
    }
    match rx.recv_timeout(timeout) {
        Ok(reply) => reply,
        Err(_) => {
            helper.pending.lock().unwrap().remove(&id);
            Err("Proton VPN took too long to answer.".into())
        }
    }
}

const SHORT: Duration = Duration::from_secs(45);
const LONG: Duration = Duration::from_secs(120);

// ── API ─────────────────────────────────────────────────────────────────────

/// The last status the client reported, without asking it (cheap).
pub fn cached_status() -> Option<Status> {
    STATUS.lock().unwrap().clone()
}

pub fn status() -> Result<Status, String> {
    let value = request("status", json!({}), SHORT)?;
    update_status(&value);
    Ok(cached_status().unwrap_or_default())
}

fn login_step(value: Value) -> LoginStep {
    match value.get("step").and_then(Value::as_str) {
        Some("done") => LoginStep::Done,
        Some("2fa") => LoginStep::TwoFactor,
        _ => LoginStep::Failed(
            value.get("error").and_then(Value::as_str).unwrap_or("Sign-in failed.").to_owned(),
        ),
    }
}

pub fn login(username: &str, password: &str) -> Result<LoginStep, String> {
    let username = username.trim();
    if username.is_empty() || password.is_empty() {
        return Ok(LoginStep::Failed("Enter your Proton username and password.".into()));
    }
    let step = login_step(request("login", json!({"username": username, "password": password}), LONG)?);
    if step == LoginStep::Done {
        let _ = status();
    }
    Ok(step)
}

pub fn submit_2fa(code: &str) -> Result<LoginStep, String> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.is_empty() {
        return Ok(LoginStep::Failed("Enter the code from your authenticator app.".into()));
    }
    let step = login_step(request("submit_2fa", json!({"code": code}), LONG)?);
    if step == LoginStep::Done {
        let _ = status();
    }
    Ok(step)
}

pub fn logout() -> Result<(), String> {
    request("logout", json!({}), LONG)?;
    let _ = status();
    Ok(())
}

pub fn countries() -> Result<Vec<Country>, String> {
    let value = request("countries", json!({}), LONG)?;
    Ok(value.as_array().map(|list| list.iter().filter_map(Country::from_value).collect()).unwrap_or_default())
}

/// Proton's countries as last fetched, from the helper's cache file. Used by
/// the search trigger, which must not wait on the network.
pub fn cached_countries() -> Vec<Country> {
    let Ok(text) = std::fs::read_to_string(cache_root().join("countries.json")) else {
        return Vec::new();
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| value.as_array().map(|list| list.iter().filter_map(Country::from_value).collect()))
        .unwrap_or_default()
}

fn target_args(target: &VpnTarget) -> Value {
    match target {
        VpnTarget::Fastest => json!({"kind": "fastest", "value": ""}),
        VpnTarget::Country(code) => json!({"kind": "country", "value": code}),
        VpnTarget::City(city) => json!({"kind": "city", "value": city}),
        VpnTarget::Server(name) => json!({"kind": "server", "value": name}),
    }
}

pub fn connect(target: &VpnTarget) -> Result<Status, String> {
    let value = request("connect", target_args(target), LONG)?;
    update_status(&value);
    let status = cached_status().unwrap_or_default();
    if status.state == "Error" {
        return Err(format!("Couldn't connect to {}: {}", target.label(), status.error));
    }
    Ok(status)
}

pub fn disconnect() -> Result<Status, String> {
    let value = request("disconnect", json!({}), LONG)?;
    update_status(&value);
    Ok(cached_status().unwrap_or_default())
}

pub fn settings() -> Result<Settings, String> {
    Ok(Settings::from_value(&request("settings", json!({}), SHORT)?))
}

pub fn set_setting(key: &str, value: Value) -> Result<Settings, String> {
    Ok(Settings::from_value(&request("set_setting", json!({"key": key, "value": value}), SHORT)?))
}

/// Run a `vpn` trigger action in the background and report the outcome as a
/// desktop notification. Signed out, it opens the sign-in window instead.
pub fn run_search_action(op: &str, encoded_target: &str) {
    let op = op.to_owned();
    let target = VpnTarget::decode(encoded_target);
    std::thread::spawn(move || {
        let logged_in = status().map(|s| s.logged_in);
        let result = match (op.as_str(), logged_in) {
            (_, Err(error)) => Err(error),
            (_, Ok(false)) => {
                glib::MainContext::default().invoke(|| crate::ui::settings_window::open_proton_vpn_popup());
                return;
            }
            ("disconnect", Ok(true)) => disconnect().map(|_| ("Proton VPN disconnected".to_owned(), String::new())),
            (_, Ok(true)) => connect(&target).map(|status| {
                let place = crate::search::proton::country_name(&status.country).unwrap_or(&status.country).to_owned();
                (
                    format!("Connected to {}", if place.is_empty() { target.label() } else { place }),
                    status.server,
                )
            }),
        };
        let (title, body) = match result {
            Ok(message) => message,
            Err(error) => ("Proton VPN".to_owned(), error),
        };
        glib::MainContext::default().invoke(move || {
            if let Some(app) = gio::Application::default() {
                let notification = gio::Notification::new(&title);
                if !body.is_empty() {
                    notification.set_body(Some(&body));
                }
                gio::prelude::ApplicationExt::send_notification(&app, Some("proton-vpn"), &notification);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_merges_partial_state_events() {
        let mut status = Status::default();
        status.merge(&json!({"logged_in": true, "account": "me", "plan": "Plus", "state": "Disconnected"}));
        status.merge(&json!({"state": "Connected", "server": "CH#242", "country": "CH", "error": ""}));
        assert!(status.logged_in && status.connected());
        assert_eq!((status.account.as_str(), status.server.as_str()), ("me", "CH#242"));
        status.merge(&json!({"state": "Disconnecting"}));
        assert!(status.busy() && !status.connected());
    }

    #[test]
    fn login_steps_and_settings_parse() {
        assert_eq!(login_step(json!({"step": "done"})), LoginStep::Done);
        assert_eq!(login_step(json!({"step": "2fa"})), LoginStep::TwoFactor);
        assert_eq!(login_step(json!({"step": "failed", "error": "nope"})), LoginStep::Failed("nope".into()));
        let settings = Settings::from_value(&json!({
            "protocol": "wireguard",
            "protocols": [{"id": "wireguard", "label": "WireGuard"}, {"id": "openvpn-udp", "label": "OpenVPN (UDP)"}],
            "killswitch": 1, "netshield": 2, "vpn_accelerator": true, "ipv6": false
        }));
        assert_eq!(settings.protocols.len(), 2);
        assert_eq!((settings.killswitch, settings.netshield), (1, 2));
        assert!(settings.vpn_accelerator && !settings.port_forwarding);
    }

    #[test]
    fn countries_and_targets_round_trip() {
        let country = Country::from_value(&json!({
            "code": "CH", "name": "Switzerland", "cities": ["Zurich"], "servers": 40, "available": true
        }))
        .unwrap();
        assert_eq!(country.cities, vec!["Zurich"]);
        assert_eq!(target_args(&VpnTarget::Country("CH".into()))["kind"], "country");
        assert_eq!(target_args(&VpnTarget::Fastest)["kind"], "fastest");
    }
}
