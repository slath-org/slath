// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath serve`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use slath_git::{Runner, RunnerConfig};
use slath_http::{AppState, StateParts, ipc};
use tracing_subscriber::EnvFilter;

use slath_core::config::LogFormat;

use crate::config::Loaded as Config;

pub fn run(config_path: &Path, migrate: bool) -> ExitCode {
    let cfg = match crate::config::load(config_path) {
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
    match rt.block_on(serve(cfg, migrate)) {
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

async fn serve(cfg: Config, migrate: bool) -> Result<(), String> {
    let hooks_dir = prepare_dirs(&cfg)?;
    let mut pc = slath_db::PoolConfig::new(crate::config::database_url(&cfg)?);
    pc.max_connections = cfg.database.max_connections;
    let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
    if migrate {
        slath_db::migrate(&pool).await.map_err(|e| e.to_string())?;
    }
    slath_db::check_schema(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut rc = RunnerConfig::new(
        cfg.git.binary.clone(),
        cfg.storage.data_dir.join("git-home"),
    );
    rc.hooks_dir = Some(hooks_dir.clone());
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    rc.max_concurrent = cfg.limits.git_max_concurrent(cpus);
    rc.read_timeout = cfg.limits.git_read_timeout.get();
    let runner = Runner::new(rc).map_err(|e| e.to_string())?;
    let version = runner.check_version().await.map_err(|e| e.to_string())?;
    tracing::info!(%version, "git version ok");

    let master = match &cfg.secrets.master_key_file {
        Some(path) => slath_auth::MasterKey::load(path).map_err(|e| e.to_string())?,
        None => {
            let path = cfg.storage.data_dir.join("instance.key");
            tracing::warn!(path = %path.display(), "secrets.master_key_file is not set; using a generated key in data_dir");
            slath_auth::MasterKey::load_or_create(&path).map_err(|e| e.to_string())?
        }
    };
    let cfg = Arc::new(cfg.cfg);
    let services =
        slath_service::Services::new(pool.clone(), Arc::clone(&cfg), &master, runner.clone());
    let metrics: Arc<slath_http::metrics::Metrics> = Arc::default();
    let hooks: Arc<ipc::HookRegistry> = Arc::default();
    let socket = cfg.storage.run_dir.join("slath.sock");
    let ctx = Arc::new(ipc::IpcContext::new(
        Arc::clone(&hooks),
        services.clone(),
        ipc::GitPaths {
            binary: cfg.git.binary.clone(),
            home_dir: cfg.storage.data_dir.join("git-home"),
            hooks_dir: Some(hooks_dir),
        },
        Arc::clone(&metrics),
    ));
    let listener =
        ipc::bind(&socket).map_err(|e| format!("cannot bind {}: {e}", socket.display()))?;
    tokio::spawn(ipc::serve(listener, ctx));

    let state = AppState::new(
        StateParts {
            config: cfg,
            services,
            pool: pool.clone(),
            runner,
            hooks,
            ipc_socket: socket,
        },
        metrics,
    )?;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let workers = {
        let mut wc = slath_db::worker::WorkerPoolConfig::new("default");
        wc.concurrency = state.config.limits.job_workers;
        let pool_task = state
            .services
            .register_jobs(slath_db::worker::WorkerPool::new(pool.clone(), wc));
        let liveness = Arc::clone(&state.liveness);
        tokio::spawn(async move {
            let _alive = slath_http::health::workers_running(&liveness);
            pool_task.run(stop_rx).await;
        })
    };
    {
        // Periodic maintenance: purge repositories whose trash time is up.
        let svc = state.services.clone();
        let mut stop = stop_tx.subscribe();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if let Err(e) = svc.purge_deleted_repos().await {
                            tracing::warn!(error = %e, "purging deleted repositories failed");
                        }
                    }
                    _ = stop.changed() => return,
                }
            }
        });
    }
    {
        // The reconciler: at startup for recently pushed repositories, then
        // every maintenance.reconcile_interval for all of them.
        let svc = state.services.clone();
        let metrics = Arc::clone(&state.metrics);
        let interval = state
            .config
            .maintenance
            .reconcile_interval
            .get()
            .max(std::time::Duration::from_secs(1));
        let mut stop = stop_tx.subscribe();
        tokio::spawn(async move {
            let day_ago = time::OffsetDateTime::now_utc() - time::Duration::days(1);
            let mut since = Some(day_ago);
            let mut tick = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        match svc.reconcile_recent(since.take()).await {
                            Ok(n) => {
                                for _ in 0..n {
                                    metrics.inc("slath_reconcile_repairs_total", "");
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "reconciler run failed"),
                        }
                    }
                    _ = stop.changed() => return,
                }
            }
        });
    }
    let metrics_addr = state.config.server.metrics_listen.trim().to_owned();
    if !metrics_addr.is_empty() {
        let l = tokio::net::TcpListener::bind(&metrics_addr)
            .await
            .map_err(|e| format!("cannot listen on {metrics_addr} (metrics): {e}"))?;
        let app = slath_http::metrics::router(Arc::clone(&state.metrics));
        tokio::spawn(async move { axum::serve(l, app).await });
    }
    let listen = state.config.server.listen;
    let tcp = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    let addr = tcp.local_addr().map_err(|e| e.to_string())?;
    tracing::info!(%addr, "listening");
    // Machine-readable line for scripts and the test launcher.
    println!("slath: listening on http://{addr}");
    let app =
        slath_http::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let result = axum::serve(tcp, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string());
    let _ = stop_tx.send(true);
    let _ = workers.await;
    result
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
