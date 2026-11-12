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

pub fn run(config_path: &Path) -> ExitCode {
    let cfg = match crate::config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("slath: {e}");
            return ExitCode::from(2);
        }
    };
    init_logging(&cfg);
    for w in &cfg.warnings {
        tracing::warn!(warning = %w, "deprecated configuration");
    }
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
    match rt.block_on(serve(config_path, cfg)) {
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
pub(crate) fn prepare_dirs(cfg: &Config) -> Result<PathBuf, String> {
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

async fn serve(config_path: &Path, cfg: Config) -> Result<(), String> {
    let hooks_dir = prepare_dirs(&cfg)?;
    let socket_gid = crate::config::socket_gid(&cfg)?;
    let mut pc = slath_db::PoolConfig::new(crate::config::database_url(&cfg)?);
    pc.max_connections = cfg.database.max_connections;
    // Startup work (preflight, migrations, checks) may wait longer for a
    // connection than requests, which should fail fast; it gets its own pool.
    let mut boot_pc = slath_db::PoolConfig::new(crate::config::database_url(&cfg)?);
    boot_pc.max_connections = 4;
    boot_pc.acquire_timeout = std::time::Duration::from_secs(30);
    let boot = connect_patiently(&boot_pc).await?;
    crate::upgrade::upgrade_on_start(config_path, &cfg, &boot).await?;
    slath_db::check_schema(&boot)
        .await
        .map_err(|e| e.to_string())?;
    let mut rc = RunnerConfig::new(
        cfg.git.binary.clone(),
        cfg.storage.data_dir.join("git-home"),
    );
    rc.hooks_dir = Some(hooks_dir);
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    rc.max_concurrent = cfg.limits.git_max_concurrent(cpus);
    rc.read_timeout = cfg.limits.git_read_timeout.get();
    let runner = Runner::new(rc).map_err(|e| e.to_string())?;
    let version = runner.check_version().await.map_err(|e| e.to_string())?;
    tracing::info!(%version, "git version ok");

    let (master, old_keys) = crate::config::load_keys(&cfg)?;
    slath_db::secrets::register_key(&boot, master.id())
        .await
        .map_err(|e| format!("recording the master key: {e}"))?;
    boot.close().await;
    let pool = connect_patiently(&pc).await?;
    let cfg = Arc::new(cfg.cfg);
    let services =
        slath_service::Services::new(pool.clone(), Arc::clone(&cfg), &master, runner.clone())
            .with_old_keys(&master, &old_keys);
    let metrics: Arc<slath_http::metrics::Metrics> = Arc::default();
    let hooks: Arc<ipc::HookRegistry> = Arc::default();
    let socket = cfg.storage.run_dir.join("slath.sock");
    let ctx = Arc::new(ipc::IpcContext::new(
        Arc::clone(&hooks),
        services.clone(),
        runner.clone(),
        socket.clone(),
        Arc::clone(&metrics),
    ));
    let listener = ipc::bind(&socket, socket_gid).map_err(|e| {
        let hint = match &cfg.storage.socket_group {
            Some(g) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                format!(" (the server's user must be a member of group {g})")
            }
            _ => String::new(),
        };
        format!("cannot bind {}: {e}{hint}", socket.display())
    })?;
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
        // Periodic maintenance: purge repositories whose trash time is up
        // and audit events past their retention; schedule repository maintenance;
        // delete attachment and avatar files unused for the grace period.
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
                        if let Err(e) = svc.purge_audit().await {
                            tracing::warn!(error = %e, "purging old audit events failed");
                        }
                        if let Err(e) = svc.schedule_maintenance().await {
                            tracing::warn!(error = %e, "scheduling repository maintenance failed");
                        }
                        if let Err(e) = svc.purge_oidc().await {
                            tracing::warn!(error = %e, "purging expired sign-in states failed");
                        }
                        if let Err(e) = svc.purge_secret_scan().await {
                            tracing::warn!(error = %e, "purging expired push protection allowances failed");
                        }
                        if let Err(e) = svc.purge_webhook_deliveries().await {
                            tracing::warn!(error = %e, "purging old webhook deliveries failed");
                        }
                        match svc.collect_blobs().await {
                            Ok(c) if c.blobs + c.orphans > 0 => {
                                tracing::info!(blobs = c.blobs, orphans = c.orphans, "deleted unused files");
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "deleting unused files failed"),
                        }
                    }
                    _ = stop.changed() => return,
                }
            }
        });
    }
    if let Some(at) = state.config.backup.schedule {
        // Scheduled backups (daily, UTC) into <data_dir>/backups; the
        // newest `keep` scheduled backups stay.
        let svc = state.services.clone();
        let config_path = config_path.to_path_buf();
        let dir = state.config.storage.data_dir.join("backups");
        let keep = state.config.backup.keep.max(1);
        // Tests shorten the day to a few seconds.
        let test_interval = std::env::var("SLATH_TEST_BACKUP_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok());
        let mut stop = stop_tx.subscribe();
        tokio::spawn(async move {
            loop {
                let wait = test_interval.unwrap_or_else(|| {
                    let now = time::OffsetDateTime::now_utc().unix_timestamp();
                    u64::try_from(at.seconds_until_next(now)).unwrap_or(86_400)
                });
                tokio::select! {
                    () = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                    _ = stop.changed() => return,
                }
                let started = time::OffsetDateTime::now_utc();
                let name = format!(
                    "scheduled-{:04}{:02}{:02}T{:02}{:02}{:02}Z",
                    started.year(),
                    u8::from(started.month()),
                    started.day(),
                    started.hour(),
                    started.minute(),
                    started.second()
                );
                let target = dir.join(&name);
                let result = crate::backup::create_with(&config_path, &target, false, false).await;
                let outcome = slath_service::admin::ScheduledBackup {
                    at: time::OffsetDateTime::now_utc().unix_timestamp(),
                    result: match &result {
                        Ok(_) => Ok(target.display().to_string()),
                        Err(e) => Err(e.clone()),
                    },
                };
                match result {
                    Ok(_) => {
                        tracing::info!(path = %target.display(), "scheduled backup written");
                        crate::upgrade::prune_backups_named(&target, keep, "scheduled-");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "the scheduled backup failed");
                        // Leave no half-written backup behind.
                        let _ = std::fs::remove_dir_all(&target);
                    }
                }
                if let Err(e) = svc.record_scheduled_backup(&outcome).await {
                    tracing::warn!(error = %e, "recording the scheduled backup failed");
                }
            }
        });
    }
    if state.config.upgrade.check_for_updates {
        // The opt-in update check: at start, then once a day. Off by
        // default; then no request leaves the server.
        let svc = state.services.clone();
        let mut stop = stop_tx.subscribe();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        match svc.check_for_updates().await {
                            Ok(Some(c)) if c.available.is_some() => {
                                if let Some(r) = &c.available {
                                    tracing::info!(version = %r.version, "a newer Slath release is available");
                                }
                            }
                            Ok(Some(c)) => {
                                if let Some(e) = &c.error {
                                    tracing::warn!(error = %e, "the update check failed");
                                }
                            }
                            Ok(None) => {}
                            Err(e) => tracing::warn!(error = %e, "the update check failed"),
                        }
                    }
                    _ = stop.changed() => return,
                }
            }
        });
    }
    if let Err(e) = state.services.on_server_start().await {
        // Not fatal: the maintenance poller picks the flag up shortly.
        tracing::warn!(error = %e, "recording the server start failed");
    }
    {
        // Maintenance mode (set by `slath upgrade` in the database).
        let svc = state.services.clone();
        let mut stop = stop_tx.subscribe();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if let Err(e) = svc.refresh_maintenance().await {
                            tracing::warn!(error = %e, "reading the maintenance flag failed");
                        }
                    }
                    _ = stop.changed() => return,
                }
            }
        });
    }
    {
        // Background migrations: registered at startup, then worked through
        // batch by batch; checked again every minute (resume, retry).
        let svc = state.services.clone();
        let stop = stop_tx.subscribe();
        tokio::spawn(async move {
            match svc.register_background_migrations().await {
                Ok(new) if !new.is_empty() => {
                    tracing::info!(migrations = ?new, "background migrations registered");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "registering background migrations failed"),
            }
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            let mut changed = stop.clone();
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let stop = stop.clone();
                        let stopped = move || *stop.borrow();
                        if let Err(e) = svc.run_background_migrations(&stopped).await {
                            tracing::warn!(error = %e, "background migrations failed");
                        }
                    }
                    _ = changed.changed() => return,
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

/// Connects to the database, retrying for up to a minute: at startup the
/// database may still be starting (containers) or busy.
async fn connect_patiently(pc: &slath_db::PoolConfig) -> Result<slath_db::PgPool, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut wait = std::time::Duration::from_millis(250);
    loop {
        match slath_db::connect(pc).await {
            Ok(pool) => return Ok(pool),
            Err(e) if std::time::Instant::now() < deadline => {
                tracing::warn!(error = %e, "database not reachable yet; retrying");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(std::time::Duration::from_secs(5));
            }
            Err(e) => return Err(format!("connecting to the database: {e}")),
        }
    }
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
