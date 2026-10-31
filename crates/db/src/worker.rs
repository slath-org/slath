// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A pool of workers that lease and run jobs from one queue.
//!
//! Workers wake on `NOTIFY` (see [`crate::jobs::enqueue`]) and also poll at a
//! fixed interval, so a lost notification only delays work. While a handler
//! runs, a heartbeat task renews the lease.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;
use tracing::Instrument;

use crate::jobs::{self, LeasedJob, NOTIFY_CHANNEL};

/// How a handler failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// Try again later (with backoff), unless attempts are used up.
    #[error("{0}")]
    Retry(String),
    /// Don't retry; the job goes straight to `dead`.
    #[error("{0}")]
    Permanent(String),
}

pub type JobFuture = Pin<Box<dyn Future<Output = Result<(), JobError>> + Send>>;

/// Runs jobs of one `kind`.
pub trait Handler: Send + Sync + 'static {
    fn run(&self, job: LeasedJob) -> JobFuture;
}

impl<F, Fut> Handler for F
where
    F: Fn(LeasedJob) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), JobError>> + Send + 'static,
{
    fn run(&self, job: LeasedJob) -> JobFuture {
        Box::pin(self(job))
    }
}

#[derive(Clone)]
pub struct WorkerPoolConfig {
    pub queue: String,
    /// Prefix of worker IDs (`<prefix>-<n>`); include the host and PID.
    pub worker_id_prefix: String,
    pub concurrency: usize,
    pub lease: Duration,
    pub poll_interval: Duration,
}

impl WorkerPoolConfig {
    #[must_use]
    pub fn new(queue: &str) -> Self {
        Self {
            queue: queue.to_owned(),
            worker_id_prefix: format!("pid{}", std::process::id()),
            concurrency: 4,
            lease: Duration::from_secs(60),
            poll_interval: Duration::from_secs(5),
        }
    }
}

pub struct WorkerPool {
    pool: PgPool,
    cfg: WorkerPoolConfig,
    handlers: HashMap<String, Arc<dyn Handler>>,
}

impl WorkerPool {
    #[must_use]
    pub fn new(pool: PgPool, cfg: WorkerPoolConfig) -> Self {
        Self {
            pool,
            cfg,
            handlers: HashMap::new(),
        }
    }

    /// Registers the handler for jobs of `kind`. Jobs of unknown kinds fail
    /// permanently, so typos surface as dead jobs instead of silent retries.
    #[must_use]
    pub fn handle(mut self, kind: &str, handler: impl Handler) -> Self {
        self.handlers.insert(kind.to_owned(), Arc::new(handler));
        self
    }

    /// Runs until `shutdown` becomes true. In-flight jobs finish first.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let wake = Arc::new(Notify::new());
        let handlers = Arc::new(self.handlers);
        let mut tasks = JoinSet::new();
        tasks.spawn(listen(
            self.pool.clone(),
            self.cfg.queue.clone(),
            Arc::clone(&wake),
            shutdown.clone(),
        ));
        for n in 0..self.cfg.concurrency.max(1) {
            let worker = Worker {
                pool: self.pool.clone(),
                cfg: self.cfg.clone(),
                id: format!("{}-{n}", self.cfg.worker_id_prefix),
                handlers: Arc::clone(&handlers),
                wake: Arc::clone(&wake),
            };
            tasks.spawn(worker.run(shutdown.clone()));
        }
        let _ = shutdown.wait_for(|stop| *stop).await;
        wake.notify_waiters();
        while tasks.join_next().await.is_some() {}
    }
}

/// Forwards notifications for our queue to the workers. Reconnects on errors;
/// polling covers any gap.
async fn listen(
    pool: PgPool,
    queue: String,
    wake: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "job listener: connect failed");
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    _ = shutdown.changed() => return,
                }
            }
        };
        if let Err(e) = listener.listen(NOTIFY_CHANNEL).await {
            tracing::warn!(error = %e, "job listener: LISTEN failed");
            continue;
        }
        loop {
            tokio::select! {
                n = listener.recv() => match n {
                    Ok(n) if n.payload() == queue => wake.notify_waiters(),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "job listener: connection lost");
                        break;
                    }
                },
                _ = shutdown.changed() => return,
            }
        }
    }
}

struct Worker {
    pool: PgPool,
    cfg: WorkerPoolConfig,
    id: String,
    handlers: Arc<HashMap<String, Arc<dyn Handler>>>,
    wake: Arc<Notify>,
}

impl Worker {
    async fn run(self, shutdown: watch::Receiver<bool>) {
        while !*shutdown.borrow() {
            // Register interest before leasing so a notification that arrives
            // in between isn't missed.
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match jobs::lease(&self.pool, &self.cfg.queue, &self.id, self.cfg.lease).await {
                Ok(Some(job)) => {
                    self.execute(job).await;
                    continue;
                }
                Ok(None) => {
                    if let Err(e) = jobs::reap_expired(&self.pool).await {
                        tracing::warn!(error = %e, "job reaper failed");
                    }
                }
                Err(e) => tracing::warn!(worker = %self.id, error = %e, "lease failed"),
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(self.cfg.poll_interval) => {}
            }
        }
    }

    async fn execute(&self, job: LeasedJob) {
        let span =
            tracing::info_span!("job", id = job.id, kind = %job.kind, attempt = job.attempts);
        self.execute_inner(job).instrument(span).await;
    }

    async fn execute_inner(&self, job: LeasedJob) {
        let (id, kind) = (job.id, job.kind.clone());
        let Some(handler) = self.handlers.get(&kind).cloned() else {
            self.finish(
                id,
                Err(JobError::Permanent(format!(
                    "no handler for job kind {kind:?}"
                ))),
            )
            .await;
            return;
        };
        let heartbeat = {
            let (pool, worker, lease) = (self.pool.clone(), self.id.clone(), self.cfg.lease);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(lease / 3).await;
                    match jobs::heartbeat(&pool, id, &worker, lease).await {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::warn!(job = id, "lease lost; result will be discarded");
                            return;
                        }
                        Err(e) => tracing::warn!(job = id, error = %e, "heartbeat failed"),
                    }
                }
            })
        };
        // Run the handler in its own task so a panic is contained.
        let result = match tokio::spawn(handler.run(job)).await {
            Ok(r) => r,
            Err(e) => Err(JobError::Retry(format!("handler panicked: {e}"))),
        };
        heartbeat.abort();
        self.finish(id, result).await;
    }

    async fn finish(&self, id: i64, result: Result<(), JobError>) {
        let outcome = match &result {
            Ok(()) => jobs::complete(&self.pool, id, &self.id).await.map(|ok| {
                if !ok {
                    tracing::warn!(job = id, "completed after losing the lease");
                }
            }),
            Err(e) => {
                let permanent = matches!(e, JobError::Permanent(_));
                jobs::fail(&self.pool, id, &self.id, &e.to_string(), permanent)
                    .await
                    .map(|o| tracing::info!(job = id, error = %e, outcome = ?o, "job failed"))
            }
        };
        if let Err(e) = outcome {
            tracing::error!(job = id, error = %e, "recording job result failed; the lease will expire");
        }
    }
}
