// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath serve`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use slath_git::{Runner, RunnerConfig, storage};
use slath_http::{AppState, RepoEntry, ipc};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, LogFormat};

pub fn run(config_path: &Path) -> ExitCode {
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("slath: {e}");
            return ExitCode::from(2);
        }
    };
    init_logging(&cfg);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("slath: cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(serve(cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "serve failed");
            eprintln!("slath: {e}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(cfg: &Config) {
    let filter =
        EnvFilter::try_from_env("SLATH_LOG").unwrap_or_else(|_| EnvFilter::new(&cfg.log.level));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let _ = match cfg.log.format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Pretty => builder.try_init(),
    };
}

/// Prepares `<data_dir>` and the managed hooks directory. The hooks are
/// symlinks to this binary; `main` dispatches on their names.
fn prepare_dirs(cfg: &Config) -> Result<PathBuf, String> {
    let data = &cfg.storage.data_dir;
    for dir in [
        data.join("git-home"),
        data.join("repos"),
        data.join("hooks"),
    ] {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot locate the slath binary: {e}"))?;
    let hooks = data.join("hooks");
    for name in ["pre-receive", "post-receive"] {
        let link = hooks.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link)
            .map_err(|e| format!("cannot install hook {}: {e}", link.display()))?;
    }
    Ok(hooks)
}

async fn serve(cfg: Config) -> Result<(), String> {
    let hooks_dir = prepare_dirs(&cfg)?;
    let mut rc = RunnerConfig::new(
        cfg.git.binary.clone(),
        cfg.storage.data_dir.join("git-home"),
    );
    rc.hooks_dir = Some(hooks_dir);
    let runner = Runner::new(rc).map_err(|e| e.to_string())?;
    let version = runner.check_version().await.map_err(|e| e.to_string())?;
    tracing::info!(%version, "git version ok");

    let mut repos = Vec::new();
    for r in &cfg.spike.repos {
        let git_dir = storage::repo_path(&cfg.storage.data_dir, r.id);
        if !git_dir.exists() {
            storage::create_bare(&runner, &cfg.storage.data_dir, r.id, &r.default_branch)
                .await
                .map_err(|e| format!("cannot create repository {}/{}: {e}", r.namespace, r.name))?;
            tracing::info!(namespace = %r.namespace, name = %r.name, "repository created");
        }
        repos.push(RepoEntry {
            namespace: r.namespace.to_string(),
            name: r.name.to_string(),
            git_dir,
        });
    }

    let socket = cfg.storage.run_dir.join("slath.sock");
    let listener =
        ipc::bind(&socket).map_err(|e| format!("cannot bind {}: {e}", socket.display()))?;
    let hooks = Arc::new(ipc::HookRegistry::default());
    tokio::spawn(ipc::serve(listener, Arc::clone(&hooks)));

    let state = AppState::new(runner, repos, cfg.spike.token, hooks, socket);
    let tcp = tokio::net::TcpListener::bind(cfg.server.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", cfg.server.listen))?;
    let addr = tcp.local_addr().map_err(|e| e.to_string())?;
    tracing::info!(%addr, "listening");
    // Machine-readable line for scripts and the test launcher.
    println!("slath: listening on http://{addr}");
    axum::serve(tcp, slath_http::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
}
