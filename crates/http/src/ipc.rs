// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The IPC server that the helper commands talk to: `slath hook` during
//! pushes, and `slath ssh-keys`/`slath ssh-serve` for SSH.
//!
//! The socket lives in the `run_dir` (mode 0700) and is itself mode 0600.
//! Each transport request mints a hook token bound to who pushes, to which
//! repository, and how; hooks must present it, so a token can't be used for
//! another repository or after the request ends. Every answer comes from
//! the service layer, which authorizes it.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slath_git::ipc::{AuthorizedKey, MAX_MESSAGE, Request, Response, TransportGrant};
use slath_git::transport::Service;
use slath_service::Services;
use slath_service::push::{PushGrant, Transport};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::metrics::Metrics;

/// How long a hook token stays valid (the transport idle timeout).
pub const HOOK_TOKEN_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
struct Entry {
    grant: PushGrant,
    expires: Instant,
}

/// In-memory registry of live hook tokens.
#[derive(Debug, Default)]
pub struct HookRegistry {
    grants: Mutex<HashMap<String, Entry>>,
}

/// Removes its token from the registry when dropped (end of the request).
#[derive(Debug)]
pub struct HookToken {
    token: String,
    registry: Option<Arc<HookRegistry>>,
}

impl HookToken {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.token
    }

    /// Keeps the token alive until it expires, for requests that outlive the
    /// minting call (SSH sessions run in a separate process).
    #[must_use]
    pub fn detach(mut self) -> String {
        self.registry = None;
        std::mem::take(&mut self.token)
    }
}

impl Drop for HookToken {
    fn drop(&mut self) {
        if let Some(registry) = &self.registry
            && let Ok(mut g) = registry.grants.lock()
        {
            g.remove(&self.token);
        }
    }
}

impl HookRegistry {
    /// Mints a random 256-bit token for one transport request.
    pub fn mint(self: &Arc<Self>, grant: PushGrant, ttl: Duration) -> std::io::Result<HookToken> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?;
        grants.retain(|_, g| g.expires > Instant::now());
        grants.insert(
            token.clone(),
            Entry {
                grant,
                expires: Instant::now() + ttl,
            },
        );
        Ok(HookToken {
            token,
            registry: Some(Arc::clone(self)),
        })
    }

    fn lookup(&self, token: &str) -> Option<PushGrant> {
        let grants = self.grants.lock().ok()?;
        grants
            .get(token)
            .filter(|g| g.expires > Instant::now())
            .map(|e| e.grant.clone())
    }
}

/// Where Git lives, as reported to `ssh-serve`.
#[derive(Debug, Clone)]
pub struct GitPaths {
    pub binary: PathBuf,
    pub home_dir: PathBuf,
    pub hooks_dir: Option<PathBuf>,
}

/// A token bucket for key lookups (sshd calls `ssh-keys` for every key a
/// client offers).
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// State the IPC server answers from.
pub struct IpcContext {
    pub hooks: Arc<HookRegistry>,
    pub services: Services,
    pub git: GitPaths,
    pub metrics: Arc<Metrics>,
    lookups: Mutex<Bucket>,
}

impl IpcContext {
    #[must_use]
    pub fn new(
        hooks: Arc<HookRegistry>,
        services: Services,
        git: GitPaths,
        metrics: Arc<Metrics>,
    ) -> Self {
        let rate = f64::from(services.config.limits.ssh_key_lookups_per_second.max(1));
        Self {
            hooks,
            services,
            git,
            metrics,
            lookups: Mutex::new(Bucket {
                tokens: rate,
                last: Instant::now(),
            }),
        }
    }

    fn allow_lookup(&self) -> bool {
        let rate = f64::from(
            self.services
                .config
                .limits
                .ssh_key_lookups_per_second
                .max(1),
        );
        let Ok(mut b) = self.lookups.lock() else {
            return false;
        };
        let now = Instant::now();
        b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * rate).min(rate);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Binds the IPC socket at `path`, replacing a stale one, with mode 0600.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accepts connections until the listener fails.
pub async fn serve(listener: UnixListener, ctx: Arc<IpcContext>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, &ctx).await {
                        tracing::warn!(error = %e, "ipc connection failed");
                    }
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "ipc accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_conn(stream: UnixStream, ctx: &IpcContext) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read.take(MAX_MESSAGE as u64));
    let mut line = Vec::new();
    let read_line = reader.read_until(b'\n', &mut line);
    tokio::time::timeout(Duration::from_secs(10), read_line)
        .await
        .map_err(|_| std::io::Error::other("ipc read timeout"))??;
    let response = match serde_json::from_slice::<Request>(&line) {
        Ok(req) => handle(req, ctx).await,
        Err(_) => Response::deny("slath: malformed request"),
    };
    let mut out = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
    out.push(b'\n');
    write.write_all(&out).await
}

const NOT_FOUND: &str = "slath: repository not found";

async fn handle(req: Request, ctx: &IpcContext) -> Response {
    match req {
        Request::PreReceive {
            token,
            updates,
            quarantine,
        } => {
            let Some(grant) = ctx.hooks.lookup(&token) else {
                ctx.metrics
                    .inc("slath_hook_denials_total", "reason=\"invalid_token\"");
                return Response::deny("slath: push rejected: invalid or expired hook token");
            };
            let v = ctx
                .services
                .pre_receive(&grant, &updates, &quarantine)
                .await;
            for r in &v.denied {
                ctx.metrics.inc(
                    "slath_hook_denials_total",
                    &format!("reason=\"{}\"", r.as_str()),
                );
            }
            Response {
                allow: v.denied.is_empty(),
                messages: v.messages,
                key: None,
                transport: None,
            }
        }
        Request::PostReceive { token, updates } => {
            let Some(grant) = ctx.hooks.lookup(&token) else {
                return Response::deny("slath: invalid or expired hook token");
            };
            Response {
                messages: ctx.services.post_receive(&grant, &updates).await,
                ..Response::allow()
            }
        }
        Request::LookupKey { fingerprint } => {
            if !ctx.allow_lookup() {
                tracing::warn!("ssh key lookups rate limited");
                return Response::deny("rate limited");
            }
            match ctx.services.ssh_lookup(&fingerprint).await {
                Ok(Some(k)) => Response {
                    key: Some(AuthorizedKey {
                        key_id: k.key_id.to_string(),
                        key_type: k.key_type,
                        key_base64: k.key_base64,
                    }),
                    ..Response::allow()
                },
                Ok(None) => Response::deny("unknown key"),
                Err(e) => {
                    tracing::error!(error = %e, "ssh key lookup failed");
                    // Fail closed: sshd treats the error like an unknown key.
                    Response::deny("lookup failed")
                }
            }
        }
        Request::AuthorizeTransport {
            key_id,
            service,
            namespace,
            repo,
        } => authorize_transport(ctx, &key_id, &service, &namespace, &repo).await,
    }
}

/// Unknown keys, unknown repositories, and repositories the key may not use
/// all get the same answer, so SSH can't be used to probe for repositories.
async fn authorize_transport(
    ctx: &IpcContext,
    key_id: &str,
    service: &str,
    namespace: &str,
    repo: &str,
) -> Response {
    let Some(service) = Service::from_name(service) else {
        return Response::deny("slath: unsupported command");
    };
    let Ok(key_id) = key_id.parse() else {
        return Response::deny(NOT_FOUND);
    };
    let subject = match ctx.services.ssh_subject(key_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Response::deny(NOT_FOUND),
        Err(e) => {
            tracing::error!(error = %e, "ssh subject lookup failed");
            return Response::deny("slath: internal error");
        }
    };
    let access = match ctx
        .services
        .transport_access(&subject, namespace, repo, service.is_write())
        .await
    {
        Ok(a) => a,
        Err(slath_service::ServiceError::NotFound | slath_service::ServiceError::Forbidden) => {
            return Response::deny(NOT_FOUND);
        }
        Err(e) => {
            tracing::error!(error = %e, "ssh authorization failed");
            return Response::deny("slath: internal error");
        }
    };
    let hook_token = if service.is_write() {
        let grant = PushGrant {
            subject,
            repo_id: access.repo.id,
            transport: Transport::Ssh,
        };
        match ctx.hooks.mint(grant, HOOK_TOKEN_TTL) {
            Ok(t) => Some(t.detach()),
            Err(e) => {
                tracing::error!(error = %e, "minting hook token failed");
                return Response::deny("slath: internal error");
            }
        }
    } else {
        None
    };
    let path = |p: &Path| p.to_string_lossy().into_owned();
    Response {
        transport: Some(TransportGrant {
            git_dir: path(&ctx.services.repo_path(access.repo.id)),
            git_binary: path(&ctx.git.binary),
            home_dir: path(&ctx.git.home_dir),
            hooks_dir: ctx.git.hooks_dir.as_deref().map(path),
            hook_token,
            max_input_bytes: service
                .is_write()
                .then(|| ctx.services.config.limits.push_max_bytes.get()),
        }),
        ..Response::allow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slath_core::authz::Subject;
    use slath_core::ids::RepoId;

    fn grant() -> PushGrant {
        PushGrant {
            subject: Subject::Anonymous,
            repo_id: RepoId::new(),
            transport: Transport::Http,
        }
    }

    #[test]
    fn tokens_are_random_expire_and_die_with_their_request() {
        let reg: Arc<HookRegistry> = Arc::default();
        let g = grant();
        let live = reg.mint(g.clone(), Duration::from_secs(60)).unwrap();
        let other = reg.mint(grant(), Duration::from_secs(60)).unwrap();
        assert_eq!(live.as_str().len(), 64);
        assert_ne!(live.as_str(), other.as_str());
        let found = reg.lookup(live.as_str()).unwrap();
        assert_eq!(
            found.repo_id, g.repo_id,
            "a token is bound to its repository"
        );
        assert!(reg.lookup("bogus").is_none());
        let s = live.as_str().to_owned();
        drop(live);
        assert!(reg.lookup(&s).is_none(), "the token dies with its request");
        let expired = reg.mint(grant(), Duration::ZERO).unwrap();
        assert!(reg.lookup(expired.as_str()).is_none());
        let detached = reg.mint(grant(), Duration::from_secs(60)).unwrap().detach();
        assert!(
            reg.lookup(&detached).is_some(),
            "detached tokens live until they expire"
        );
    }
}
