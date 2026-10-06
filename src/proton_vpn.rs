//! Small adapter for Proton's official Linux CLI. Spotty never handles account
//! credentials; sign-in remains in Proton's own interactive terminal flow.

use std::process::Output;

fn cli() -> std::process::Command {
    crate::app::host_process("protonvpn")
}

/// Probe only the official CLI executable. No package is installed by Spotty.
pub fn available() -> bool {
    cli().arg("--help").stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success())
}

fn run(args: &[&str]) -> Result<String, String> {
    let output = cli().args(args).output().map_err(|e| {
        format!("Could not start the Proton VPN CLI: {e}")
    })?;
    let succeeded = output.status.success();
    let text = output_text(output);
    if succeeded { Ok(text) } else { Err(text) }
}

fn output_text(output: Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        if !text.is_empty() { text.push('\n'); }
        text.push_str(&stderr);
    }
    if text.is_empty() {
        text = if output.status.success() {
            "Command completed.".into()
        } else {
            format!("Proton VPN CLI exited with {}.", output.status)
        };
    }
    text.chars().take(3000).collect()
}

pub fn status() -> Result<String, String> {
    if !available() {
        return Err("The Proton VPN CLI is not installed. Install it from Proton's official Linux instructions, then reopen this panel.".into());
    }
    run(&["status"])
}

/// Invoke only fixed Proton CLI subcommands. Arguments never pass through a shell.
pub fn action(action: Action) -> Result<String, String> {
    if !available() {
        return Err("The Proton VPN CLI is not installed. Install it from Proton's official Linux instructions, then reopen this panel.".into());
    }
    match action {
        Action::Connect => run(&["connect"]),
        Action::Disconnect => run(&["disconnect"]),
        Action::SignOut => run(&["signout"]),
    }
}

#[derive(Clone, Copy)]
pub enum Action { Connect, Disconnect, SignOut }
