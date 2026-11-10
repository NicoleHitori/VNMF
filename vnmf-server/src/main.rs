// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmf-server — the VNMF library server.

use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rusqlite::OptionalExtension;
use vnmf_server::{AppState, Config, auth, library};

#[derive(Parser)]
#[command(name = "vnmf-server", version, long_version = vnmf::LONG_VERSION, author = vnmf::AUTHORS, about = "VNMF library server")]
#[command(after_help = "Examples:\n  \
    vnmf-server user add nicole --admin        # the first user (asks for the password)\n  \
    vnmf-server --library ~/Manga              # serve ~/Manga (read-only) and the uploads\n  \
    vnmf-server --listen 0.0.0.0:8470 --scan-interval 60\n\n\
Uploaded books are stored in <data>/library. The API is under /api/v1 (see docs/SERVER.md).")]
struct Args {
    /// Data folder: database, uploaded books, caches
    #[arg(long, env = "VNMF_DATA", global = true)]
    data: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Address to listen on
    #[arg(long, env = "VNMF_LISTEN", default_value = "127.0.0.1:8470")]
    listen: SocketAddr,

    /// Additional folders with .vnmf files (recursive; repeat for several)
    #[arg(long = "library", env = "VNMF_LIBRARIES", value_delimiter = ':')]
    libraries: Vec<PathBuf>,

    /// Allow the server to change and delete files in the --library folders (metadata, covers,
    /// protection, repair). Without it only uploaded books can be changed
    #[arg(long)]
    writable_libraries: bool,

    /// Maximum upload size, MiB
    #[arg(long, default_value_t = 4096)]
    max_upload_mib: u64,

    /// Rescan the libraries every N minutes (0 — only at startup and on request)
    #[arg(long, default_value_t = 0, value_name = "MINUTES")]
    scan_interval: u64,

    /// Fully check every book (all data and the redundancy) at least every N days, in the background,
    /// so that silent disk damage is found while it can still be repaired (0 — only on request)
    #[arg(long, default_value_t = 7, value_name = "DAYS", env = "VNMF_VERIFY_INTERVAL")]
    verify_interval: u64,

    /// Allow browser apps from this origin (CORS), e.g. http://localhost:5173; repeat for several
    #[arg(long = "cors-origin")]
    cors_origins: Vec<String>,

    /// Mark session cookies Secure (when served over HTTPS, e.g. behind a reverse proxy)
    #[arg(long)]
    secure_cookies: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (the default)
    Serve,
    /// Manage users
    #[command(subcommand)]
    User(UserCommand),
    /// Index the libraries once and exit
    Scan,
}

#[derive(Subcommand)]
enum UserCommand {
    /// Create a user (the password is asked for, or taken from VNMF_PASSWORD)
    Add {
        name: String,
        /// Administrator: uploads, changes, users
        #[arg(long)]
        admin: bool,
    },
    /// Set a new password (ends the user's sessions)
    Passwd { name: String },
    /// List users
    List,
    /// Delete a user with their progress and bookmarks
    Remove { name: String },
}

fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("VNMF_LOG").unwrap_or_else(|_| "vnmf_server=info,tower_http=info".into()))
        .init();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vnmf-server: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn default_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("vnmf-server")
}

fn run(args: Args) -> Result<()> {
    let s = args.serve;
    let mut config = Config::new(args.data.unwrap_or_else(default_data_dir));
    config.libraries = s.libraries.into_iter().filter(|p| !p.as_os_str().is_empty()).map(|p| std::path::absolute(&p).unwrap_or(p)).collect();
    config.writable_libraries = s.writable_libraries;
    config.max_upload = s.max_upload_mib.saturating_mul(1 << 20);
    config.scan_interval = (s.scan_interval > 0).then(|| Duration::from_secs(s.scan_interval * 60));
    config.verify_interval = (s.verify_interval > 0).then(|| Duration::from_secs(s.verify_interval * 24 * 3600));
    config.cors_origins = s.cors_origins;
    config.secure_cookies = s.secure_cookies;
    let state = AppState::new(config).context("opening the data folder")?;

    match args.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(state, s.listen),
        Command::Scan => {
            library::scan(&state);
            let status = state.jobs.scan.lock().map(|s| s.clone()).unwrap_or_default();
            println!("{} files: {} added, {} updated, {} missing", status.total, status.added, status.updated, status.missing);
            for e in status.errors {
                println!("  {e}");
            }
            Ok(())
        }
        Command::User(cmd) => user(&state, cmd),
    }
}

fn serve(state: AppState, listen: SocketAddr) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let users: i64 = state.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)).await.map_err(|e| anyhow::anyhow!(e.message))?;
        if users == 0 {
            tracing::warn!("there are no users yet — create one: vnmf-server user add NAME --admin");
        }
        tracing::info!("data folder: {}", state.config.data_dir.display());
        for dir in state.config.roots() {
            tracing::info!("library: {}", dir.display());
        }
        library::start_scan(&state);
        if let Some(every) = state.config.scan_interval {
            let st = state.clone();
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(every);
                timer.tick().await;
                loop {
                    timer.tick().await;
                    library::start_scan(&st);
                }
            });
        }
        if let Some(every) = state.config.verify_interval {
            // Hourly: books not fully checked for `every` (or since they changed) are checked. A
            // restart doesn't reset it — the time of each book's last check is in the database.
            let st = state.clone();
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    timer.tick().await;
                    library::start_verify_due(&st, every);
                }
            });
        }
        let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("listening on {listen}"))?;
        tracing::info!("listening on http://{listen}");
        axum::serve(listener, vnmf_server::app(state)).with_graceful_shutdown(shutdown()).await?;
        Ok(())
    })
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
    tracing::info!("shutting down");
}

/// A password from `VNMF_PASSWORD`, or asked for twice on the terminal (without echo), or one line of stdin.
fn read_password() -> Result<String> {
    if let Ok(p) = std::env::var("VNMF_PASSWORD") {
        return Ok(p);
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    let ask = |prompt: &str| -> Result<String> {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let _ = std::process::Command::new("stty").arg("-echo").stdin(std::process::Stdio::inherit()).status();
        let mut line = String::new();
        let read = stdin.lock().read_line(&mut line);
        let _ = std::process::Command::new("stty").arg("echo").stdin(std::process::Stdio::inherit()).status();
        println!();
        read?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    };
    let first = ask("Password: ")?;
    if ask("Repeat: ")? != first {
        bail!("the passwords don't match");
    }
    Ok(first)
}

fn user(state: &AppState, cmd: UserCommand) -> Result<()> {
    let db = |f: &dyn Fn(&mut rusqlite::Connection) -> vnmf_server::error::ApiResult<()>| state.db.blocking(|c| f(c)).map_err(|e| anyhow::anyhow!(e.message));
    match cmd {
        UserCommand::Add { name, admin } => {
            let password = read_password()?;
            db(&|c| auth::create_user(c, &name, &password, admin).map(|_| ()))?;
            println!("user {name} created{}", if admin { " (administrator)" } else { "" });
        }
        UserCommand::Passwd { name } => {
            let password = read_password()?;
            auth::check_new_password(&password).map_err(|e| anyhow::anyhow!(e.message))?;
            let hash = auth::hash_password(&password)?;
            db(&|c| {
                let id: Option<i64> = c.query_row("SELECT id FROM users WHERE name = ?1", [&name], |r| r.get(0)).optional()?;
                let id = id.ok_or_else(|| vnmf_server::error::ApiError::not_found(format!("user {name}")))?;
                c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", rusqlite::params![hash, id])?;
                c.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
                Ok(())
            })?;
            println!("password of {name} changed");
        }
        UserCommand::List => {
            db(&|c| {
                let mut stmt = c.prepare("SELECT name, is_admin FROM users ORDER BY name")?;
                for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))? {
                    let (name, admin) = row?;
                    println!("{name}{}", if admin { "  (administrator)" } else { "" });
                }
                Ok(())
            })?;
        }
        UserCommand::Remove { name } => {
            db(&|c| {
                if c.execute("DELETE FROM users WHERE name = ?1", [&name])? == 0 {
                    return Err(vnmf_server::error::ApiError::not_found(format!("user {name}")));
                }
                Ok(())
            })?;
            println!("user {name} deleted");
        }
    }
    Ok(())
}
