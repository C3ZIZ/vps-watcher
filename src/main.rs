//! vps-watcher: a tiny, fast, secure monitor for a single VPS.

mod alert;
mod auth;
mod collect;
mod config;
mod log;
mod net;
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
  vps-watcher config               Check settings and print them with secrets hidden
  vps-watcher test-alert           Send a test alert to the email and webhook in the settings
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
        ["config"] => {
            let cfg = load()?;
            let file = env_file.as_deref().map_or(config::DEFAULT_ENV_FILE.into(), |p| p.display().to_string());
            println!("Settings are valid. Values come from the environment and {file}; secrets are hidden.\n");
            for (k, v) in cfg.summary() {
                println!("  {k:<14} {v}");
            }
            Ok(())
        }
        ["test-alert"] => test_alert(load()?),
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

/// Sends one test notification from the command line and reports each channel.
fn test_alert(cfg: config::Config) -> Result<(), String> {
    let host = collect::hostname();
    // No data dir: a root-run CLI must not create files the service user cannot read.
    let n = alert::Notifier::new(&cfg.alerts, None, &host, cfg.domain.as_deref());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    let results = rt.block_on(n.test(&alert::Context::default()))?;
    let mut failed = false;
    for (channel, r) in results {
        match r {
            Ok(()) => println!("{channel}: sent"),
            Err(e) => {
                failed = true;
                println!("{channel}: failed: {e}");
            }
        }
    }
    if failed { Err("some alerts were not delivered".into()) } else { Ok(()) }
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
