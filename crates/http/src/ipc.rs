// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The IPC server that the helper commands talk to: `slath hook` during
//! pushes, and `slath ssh-keys`/`slath ssh-serve` for SSH.
//!
//! The socket lives in the `run_dir`. Without a socket group, only the
//! server's own user may connect (directory 0700, socket 0600). With one
//! (`storage.socket_group`, the SSH login's group), its members may connect
//! too (directory 0710, socket 0660); they get no other access, because Git
//! for SSH runs here, on the descriptors `ssh-serve` passes in.
//!
//! Each transport request mints a hook token bound to who pushes, to which
//! repository, and how; hooks must present it, so a token can't be used for
//! another repository or after the request ends. Every answer comes from
//! the service layer, which authorizes it.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slath_git::Runner;
use slath_git::ipc::{self as proto, AuthorizedKey, MAX_MESSAGE, Request, Response};
use slath_git::transport::{self, HookEnv, Service};
use slath_service::Services;
use slath_service::push::{PushGrant, Transport};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Interest};
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
    /// Runs Git for SSH sessions.
    pub runner: Runner,
    /// This socket's path, for the hooks of SSH pushes.
    pub socket: PathBuf,
    pub metrics: Arc<Metrics>,
    lookups: Mutex<Bucket>,
}

impl IpcContext {
    #[must_use]
    pub fn new(
        hooks: Arc<HookRegistry>,
        services: Services,
        runner: Runner,
        socket: PathBuf,
        metrics: Arc<Metrics>,
    ) -> Self {
        let rate = f64::from(services.config.limits.ssh_key_lookups_per_second.max(1));
        Self {
            hooks,
            services,
            runner,
            socket,
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

/// Binds the IPC socket at `path`, replacing a stale one. Without `group`,
/// the directory gets mode 0700 and the socket 0600; with it, both are
/// handed to that group, with 0710 and 0660. Giving a group away needs this
/// process to be a member of it.
pub fn bind(path: &Path, group: Option<u32>) -> std::io::Result<UnixListener> {
    let (dir_mode, socket_mode) = if group.is_some() {
        (0o710, 0o660)
    } else {
        (0o700, 0o600)
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        set_group(dir, group)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(dir_mode))?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    set_group(path, group)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(socket_mode))?;
    Ok(listener)
}

fn set_group(path: &Path, group: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    match group {
        Some(gid) if std::fs::metadata(path)?.gid() != gid => {
            std::os::unix::fs::chown(path, None, Some(gid))
        }
        _ => Ok(()),
    }
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

async fn handle_conn(mut stream: UnixStream, ctx: &IpcContext) -> std::io::Result<()> {
    let (line, trailing) = {
        let (read, _) = stream.split();
        let mut reader = BufReader::new(read.take(MAX_MESSAGE as u64));
        let mut line = Vec::new();
        let read_line = reader.read_until(b'\n', &mut line);
        tokio::time::timeout(Duration::from_secs(10), read_line)
            .await
            .map_err(|_| std::io::Error::other("ipc read timeout"))??;
        (line, !reader.buffer().is_empty())
    };
    // Clients wait for the answer before sending anything else.
    let request = if trailing {
        None
    } else {
        serde_json::from_slice::<Request>(&line).ok()
    };
    let response = match request {
        Some(Request::SshSession {
            key_id,
            service,
            namespace,
            repo,
            git_protocol,
        }) => {
            let session = SshRequest {
                key_id: &key_id,
                service: &service,
                namespace: &namespace,
                repo: &repo,
                git_protocol: git_protocol.as_deref(),
            };
            return ssh_session(stream, ctx, &session).await;
        }
        Some(req) => handle(req, ctx).await,
        None => Response::deny("slath: malformed request"),
    };
    write_response(&mut stream, &response).await
}

async fn write_response(stream: &mut UnixStream, response: &Response) -> std::io::Result<()> {
    let mut out = serde_json::to_vec(response).map_err(std::io::Error::other)?;
    out.push(b'\n');
    stream.write_all(&out).await
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
            if let Some(reason) = ctx.services.maintenance().reason() {
                ctx.metrics
                    .inc("slath_hook_denials_total", "reason=\"maintenance\"");
                return Response::deny(format!(
                    "slath: push rejected: maintenance in progress ({reason}); try again in a few minutes"
                ));
            }
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
                ..Response::allow()
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
        Request::Status => Response {
            status: Some(Box::new(slath_git::ipc::ServerStatus {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                schema: slath_db::REQUIRED_SCHEMA,
                maintenance: ctx.services.maintenance().is_on(),
            })),
            ..Response::allow()
        },
        // Handled by `handle_conn`, which owns the connection.
        Request::SshSession { .. } => Response::deny("slath: malformed request"),
    }
}

struct SshRequest<'a> {
    key_id: &'a str,
    service: &'a str,
    namespace: &'a str,
    repo: &'a str,
    git_protocol: Option<&'a str>,
}

/// What an authorized SSH request runs.
struct SshGrant {
    service: Service,
    git_dir: PathBuf,
    /// Present for `git-receive-pack`; valid until the session ends.
    hook_token: Option<HookToken>,
}

/// How long `ssh-serve` has to pass its descriptors after the go-ahead.
const FD_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs one SSH session: authorize, receive the SSH channel's descriptors,
/// run Git on them, and report its exit code. If `ssh-serve` goes away
/// first, Git is killed.
async fn ssh_session(
    mut stream: UnixStream,
    ctx: &IpcContext,
    req: &SshRequest<'_>,
) -> std::io::Result<()> {
    let grant = match authorize_ssh(ctx, req).await {
        Ok(g) => g,
        Err(denied) => return write_response(&mut stream, &denied).await,
    };
    write_response(&mut stream, &Response::allow()).await?;
    let recv = stream.async_io(Interest::READABLE, || {
        proto::recv_fds(&stream).map_err(|e| match e {
            proto::IpcError::Io(e) => e,
            other => std::io::Error::other(other.to_string()),
        })
    });
    let fds = match tokio::time::timeout(FD_TIMEOUT, recv).await {
        Ok(Ok(fds)) => fds,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "ssh session sent no usable descriptors");
            return write_response(&mut stream, &Response::deny("slath: malformed request")).await;
        }
        Err(_) => return Err(std::io::Error::other("ipc descriptor timeout")),
    };
    let Ok(stdio) = <[OwnedFd; 3]>::try_from(fds) else {
        return write_response(&mut stream, &Response::deny("slath: malformed request")).await;
    };
    let hooks = grant.hook_token.as_ref().map(|t| HookEnv {
        token: t.as_str(),
        ipc_socket: &ctx.socket,
        max_input_bytes: Some(ctx.services.config.limits.push_max_bytes.get()),
    });
    let spawned = transport::spawn_ssh(
        &ctx.runner,
        &grant.git_dir,
        grant.service,
        req.git_protocol,
        hooks.as_ref(),
        stdio,
    )
    .await;
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "cannot start git for ssh");
            return write_response(&mut stream, &Response::deny("slath: internal error")).await;
        }
    };
    let status = tokio::select! {
        status = child.wait() => status,
        () = client_gone(&stream) => {
            child.kill();
            return Ok(());
        }
    };
    let code = match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            tracing::error!(error = %e, "waiting for git failed");
            1
        }
    };
    let done = Response {
        allow: code == 0,
        exit_code: Some(code),
        ..Response::allow()
    };
    write_response(&mut stream, &done).await
}

/// Resolves when the client closes its end or sends anything (it must only
/// wait for the answer).
async fn client_gone(stream: &UnixStream) {
    let mut buf = [0u8; 1];
    loop {
        if stream.readable().await.is_err() {
            return;
        }
        match stream.try_read(&mut buf) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            _ => return,
        }
    }
}

/// Unknown keys, unknown repositories, and repositories the key may not use
/// all get the same answer, so SSH can't be used to probe for repositories.
async fn authorize_ssh(ctx: &IpcContext, req: &SshRequest<'_>) -> Result<SshGrant, Response> {
    let Some(service) = Service::from_name(req.service) else {
        return Err(Response::deny("slath: unsupported command"));
    };
    let Ok(key_id) = req.key_id.parse() else {
        return Err(Response::deny(NOT_FOUND));
    };
    let subject = match ctx.services.ssh_subject(key_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Err(Response::deny(NOT_FOUND)),
        Err(e) => {
            tracing::error!(error = %e, "ssh subject lookup failed");
            return Err(Response::deny("slath: internal error"));
        }
    };
    let access = match ctx
        .services
        .transport_access(&subject, req.namespace, req.repo, service.is_write())
        .await
    {
        Ok(a) => a,
        Err(slath_service::ServiceError::NotFound | slath_service::ServiceError::Forbidden) => {
            return Err(Response::deny(NOT_FOUND));
        }
        Err(e) => {
            tracing::error!(error = %e, "ssh authorization failed");
            return Err(Response::deny("slath: internal error"));
        }
    };
    let hook_token = if service.is_write() {
        let grant = PushGrant {
            subject,
            repo_id: access.repo.id,
            transport: Transport::Ssh,
        };
        match ctx.hooks.mint(grant, HOOK_TOKEN_TTL) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::error!(error = %e, "minting hook token failed");
                return Err(Response::deny("slath: internal error"));
            }
        }
    } else {
        None
    };
    Ok(SshGrant {
        service,
        git_dir: ctx.services.repo_path(access.repo.id),
        hook_token,
    })
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
    }

    #[tokio::test]
    async fn socket_modes_with_and_without_a_group() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let gid = std::fs::metadata(dir.path()).unwrap().gid();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().mode() & 0o7777;

        let private = dir.path().join("private").join("slath.sock");
        let _l = bind(&private, None).unwrap();
        assert_eq!(mode(private.parent().unwrap()), 0o700);
        assert_eq!(mode(&private), 0o600);

        let shared = dir.path().join("shared").join("slath.sock");
        let _l = bind(&shared, Some(gid)).unwrap();
        let meta = std::fs::metadata(&shared).unwrap();
        assert_eq!(meta.gid(), gid);
        assert_eq!(mode(&shared), 0o660);
        assert_eq!(
            mode(shared.parent().unwrap()),
            0o710,
            "searchable, not listable"
        );
        assert_eq!(
            std::fs::metadata(shared.parent().unwrap()).unwrap().gid(),
            gid
        );

        // Rebinding replaces a stale socket and tightens a loosened directory.
        std::fs::set_permissions(
            shared.parent().unwrap(),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        drop(_l);
        let _l = bind(&shared, Some(gid)).unwrap();
        assert_eq!(mode(shared.parent().unwrap()), 0o710);
    }
}
