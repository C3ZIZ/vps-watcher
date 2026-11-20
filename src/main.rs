//! vps-watcher: a tiny, fast, secure monitor for a single VPS.

mod auth;
mod collect;
mod config;
mod log;
mod proxy;
mod report;
mod store;
mod util;
mod web;

use std::path::PathBuf;
use std::process::ExitCode;

/// Release version, injected by the release build; falls back to the crate version.
pub const VERSION: &str = match option_env!("VPSW_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

const HELP: &str = "\
vps-watcher - a tiny monitor for one VPS

USAGE:
  vps-watcher [serve]              Run the monitor and web UI
  vps-watcher proxy caddy|nginx    Print a reverse proxy config for VPSW_DOMAIN
  vps-watcher reset-admin          Delete the admin so the setup page appears again
  vps-watcher version              Print the version

OPTIONS:
  --env-file <path>                Read settings from this file
                                   (default: /etc/vps-watcher/vps-watcher.env if present)

Settings are environment variables (VPSW_LISTEN, VPSW_AUTH, VPSW_DOMAIN, ...).
See https://github.com/C3ZIZ/vps-watcher for the full guide.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches the subcommand.
fn run(args: &[String]) -> Result<(), String> {
    let (env_file, rest) = split_env_file(args)?;
    let words: Vec<&str> = rest.iter().map(String::as_str).collect();
    let load = || config::Config::load(env_file.as_deref());
    match words.as_slice() {
        [] | ["serve"] => web::serve(load()?),
        ["proxy", "caddy"] => {
            println!("{}", proxy::caddy(&load()?)?);
            Ok(())
        }
        ["proxy", "nginx"] => {
            println!("{}", proxy::nginx(&load()?)?);
            Ok(())
        }
        ["proxy", ..] => Err("use 'vps-watcher proxy caddy' or 'vps-watcher proxy nginx'".into()),
        ["reset-admin"] => {
            let cfg = load()?;
            if auth::Auth::reset(&cfg.data_dir)? {
                println!("Admin removed. Restart the service (sudo systemctl restart vps-watcher),");
                println!("then open http://{} through an SSH tunnel to create a new admin.", cfg.listen);
            } else {
                println!("No admin exists yet; the setup page is already active.");
            }
            Ok(())
        }
        ["version" | "--version" | "-V"] => {
            println!("vps-watcher {VERSION}");
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{HELP}");
            Ok(())
        }
        _ => Err(format!("unknown command '{}'; run 'vps-watcher help'", rest.join(" "))),
    }
}

/// Pulls `--env-file <path>` out of the arguments.
fn split_env_file(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut file = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--env-file" {
            file = Some(it.next().ok_or("--env-file needs a path")?.into());
        } else if let Some(p) = a.strip_prefix("--env-file=") {
            file = Some(p.into());
        } else {
            rest.push(a.clone());
        }
    }
    Ok((file, rest))
}
