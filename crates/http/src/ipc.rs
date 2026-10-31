// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The IPC server that the helper commands talk to: `slath hook` during
//! pushes, and `slath ssh-keys`/`slath ssh-serve` for SSH.
//!
//! The socket lives in the `run_dir` (mode 0700) and is itself mode 0600.
//! Each transport request mints a hook token bound to its repository; hooks
//! must present it, so a token can't be used for another repository or
//! after the request ends.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slath_git::args::RefName;
use slath_git::ipc::{AuthorizedKey, MAX_MESSAGE, RefUpdate, Request, Response, TransportGrant};
use slath_git::transport::Service;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// How long a hook token stays valid (the transport idle timeout).
pub const HOOK_TOKEN_TTL: Duration = Duration::from_secs(600);

/// What a hook token grants.
#[derive(Debug, Clone)]
pub struct HookGrant {
    pub git_dir: PathBuf,
    pub expires: Instant,
}

/// In-memory registry of live hook tokens.
#[derive(Debug, Default)]
pub struct HookRegistry {
    grants: Mutex<HashMap<String, HookGrant>>,
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
    pub fn mint(self: &Arc<Self>, git_dir: PathBuf, ttl: Duration) -> std::io::Result<HookToken> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let grant = HookGrant {
            git_dir,
            expires: Instant::now() + ttl,
        };
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?;
        grants.retain(|_, g| g.expires > Instant::now());
        grants.insert(token.clone(), grant);
        Ok(HookToken {
            token,
            registry: Some(Arc::clone(self)),
        })
    }

    fn lookup(&self, token: &str) -> Option<HookGrant> {
        let grants = self.grants.lock().ok()?;
        grants
            .get(token)
            .filter(|g| g.expires > Instant::now())
            .cloned()
    }
}

/// An SSH key the spike server accepts, and the repositories it may use.
#[derive(Debug, Clone)]
pub struct SshKey {
    pub id: String,
    pub key_type: String,
    pub key_base64: String,
    /// Lowercase `(namespace, name)` pairs.
    pub repos: HashSet<(String, String)>,
}

/// Where Git lives, as reported to `ssh-serve`.
#[derive(Debug, Clone)]
pub struct GitPaths {
    pub binary: PathBuf,
    pub home_dir: PathBuf,
    pub hooks_dir: Option<PathBuf>,
}

/// State the IPC server answers from.
#[derive(Debug)]
pub struct IpcContext {
    pub hooks: Arc<HookRegistry>,
    /// Keyed by fingerprint (`SHA256:…`).
    pub keys: HashMap<String, SshKey>,
    /// Lowercase `(namespace, name)` to the repository's Git directory.
    pub repos: HashMap<(String, String), PathBuf>,
    pub git: GitPaths,
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
        Ok(req) => handle(req, ctx),
        Err(_) => Response::deny("slath: malformed request"),
    };
    let mut out = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
    out.push(b'\n');
    write.write_all(&out).await
}

fn handle(req: Request, ctx: &IpcContext) -> Response {
    match req {
        Request::PreReceive {
            token,
            updates,
            quarantine: _,
        } => {
            let Some(grant) = ctx.hooks.lookup(&token) else {
                return Response::deny("slath: push rejected: invalid or expired hook token");
            };
            pre_receive(&grant, &updates)
        }
        Request::PostReceive { token, updates } => {
            let Some(grant) = ctx.hooks.lookup(&token) else {
                return Response::deny("slath: invalid or expired hook token");
            };
            for u in &updates {
                tracing::info!(
                    repo = %grant.git_dir.display(),
                    r#ref = %String::from_utf8_lossy(&u.name),
                    old = %u.old,
                    new = %u.new,
                    "ref updated"
                );
            }
            Response::allow()
        }
        Request::LookupKey { fingerprint } => match ctx.keys.get(&fingerprint) {
            Some(k) => Response {
                key: Some(AuthorizedKey {
                    key_id: k.id.clone(),
                    key_type: k.key_type.clone(),
                    key_base64: k.key_base64.clone(),
                }),
                ..Response::allow()
            },
            None => Response::deny("unknown key"),
        },
        Request::AuthorizeTransport {
            key_id,
            service,
            namespace,
            repo,
        } => authorize_transport(ctx, &key_id, &service, &namespace, &repo),
    }
}

/// Unknown keys, unknown repositories, and repositories the key may not use
/// all get the same answer, so SSH can't be used to probe for repositories.
fn authorize_transport(
    ctx: &IpcContext,
    key_id: &str,
    service: &str,
    namespace: &str,
    repo: &str,
) -> Response {
    const NOT_FOUND: &str = "slath: repository not found";
    let Some(service) = Service::from_name(service) else {
        return Response::deny("slath: unsupported command");
    };
    let wanted = (namespace.to_lowercase(), repo.to_lowercase());
    let Some(key) = ctx.keys.values().find(|k| k.id == key_id) else {
        return Response::deny(NOT_FOUND);
    };
    let (true, Some(git_dir)) = (key.repos.contains(&wanted), ctx.repos.get(&wanted)) else {
        return Response::deny(NOT_FOUND);
    };
    let hook_token = if service.is_write() {
        match ctx.hooks.mint(git_dir.clone(), HOOK_TOKEN_TTL) {
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
            git_dir: path(git_dir),
            git_binary: path(&ctx.git.binary),
            home_dir: path(&ctx.git.home_dir),
            hooks_dir: ctx.git.hooks_dir.as_deref().map(path),
            hook_token,
        }),
        ..Response::allow()
    }
}

/// Spike policy: everything except Slath's reserved namespaces. Ref-type,
/// role, and protection checks come later.
fn pre_receive(_grant: &HookGrant, updates: &[RefUpdate]) -> Response {
    let mut messages = Vec::new();
    for u in updates {
        let name = String::from_utf8_lossy(&u.name);
        match RefName::parse(u.name.clone()) {
            Ok(r) if r.is_reserved() => {
                messages.push(format!(
                    "slath: {name}: refs/slath/ and refs/merge-requests/ are reserved"
                ));
            }
            Ok(_) => {}
            Err(why) => messages.push(format!("slath: {name}: {why}")),
        }
    }
    if messages.is_empty() {
        Response::allow()
    } else {
        messages.push("slath: push rejected".to_owned());
        Response {
            allow: false,
            messages,
            key: None,
            transport: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(name: &str) -> RefUpdate {
        RefUpdate {
            old: "0".repeat(40),
            new: "1".repeat(40),
            name: name.as_bytes().to_vec(),
        }
    }

    fn ctx() -> IpcContext {
        let repos = HashMap::from([
            (
                ("alice".into(), "demo".into()),
                PathBuf::from("/data/repos/a.git"),
            ),
            (
                ("bob".into(), "other".into()),
                PathBuf::from("/data/repos/b.git"),
            ),
        ]);
        let key = SshKey {
            id: "k1".into(),
            key_type: "ssh-ed25519".into(),
            key_base64: "AAAA".into(),
            repos: HashSet::from([("alice".into(), "demo".into())]),
        };
        IpcContext {
            hooks: Arc::default(),
            keys: HashMap::from([("SHA256:abc".into(), key)]),
            repos,
            git: GitPaths {
                binary: "/usr/bin/git".into(),
                home_dir: "/h".into(),
                hooks_dir: None,
            },
        }
    }

    #[test]
    fn reserved_refs_are_denied_atomically() {
        let ctx = ctx();
        let t = ctx
            .hooks
            .mint("/x".into(), Duration::from_secs(60))
            .unwrap();
        let req = |names: &[&str]| Request::PreReceive {
            token: t.as_str().to_owned(),
            updates: names.iter().map(|n| update(n)).collect(),
            quarantine: Default::default(),
        };
        assert!(handle(req(&["refs/heads/main", "refs/tags/v1"]), &ctx).allow);
        let r = handle(req(&["refs/heads/main", "refs/slath/x"]), &ctx);
        assert!(!r.allow);
        assert!(r.messages[0].contains("refs/slath/x"));
        assert!(!handle(req(&["refs/merge-requests/1/head"]), &ctx).allow);
    }

    #[test]
    fn tokens_are_checked_and_expire() {
        let ctx = ctx();
        let req = |token: &str| Request::PreReceive {
            token: token.to_owned(),
            updates: vec![update("refs/heads/main")],
            quarantine: Default::default(),
        };
        assert!(!handle(req("bogus"), &ctx).allow);
        let expired = ctx.hooks.mint("/x".into(), Duration::ZERO).unwrap();
        assert!(!handle(req(expired.as_str()), &ctx).allow);
        let live = ctx
            .hooks
            .mint("/x".into(), Duration::from_secs(60))
            .unwrap();
        let s = live.as_str().to_owned();
        assert_eq!(s.len(), 64);
        assert!(handle(req(&s), &ctx).allow);
        drop(live);
        assert!(
            !handle(req(&s), &ctx).allow,
            "token must die with its request"
        );
    }

    #[test]
    fn key_lookup() {
        let ctx = ctx();
        let r = handle(
            Request::LookupKey {
                fingerprint: "SHA256:abc".into(),
            },
            &ctx,
        );
        assert_eq!(r.key.map(|k| k.key_id), Some("k1".into()));
        assert!(
            handle(
                Request::LookupKey {
                    fingerprint: "SHA256:zzz".into()
                },
                &ctx
            )
            .key
            .is_none()
        );
    }

    #[test]
    fn transport_authorization() {
        let ctx = ctx();
        let auth = |key: &str, svc: &str, ns: &str, repo: &str| {
            handle(
                Request::AuthorizeTransport {
                    key_id: key.into(),
                    service: svc.into(),
                    namespace: ns.into(),
                    repo: repo.into(),
                },
                &ctx,
            )
        };
        let read = auth("k1", "git-upload-pack", "Alice", "Demo");
        assert!(read.allow);
        assert!(
            read.transport
                .as_ref()
                .is_some_and(|t| t.hook_token.is_none())
        );
        let write = auth("k1", "git-receive-pack", "alice", "demo");
        let token = write.transport.and_then(|t| t.hook_token).unwrap();
        assert!(
            ctx.hooks.lookup(&token).is_some(),
            "detached token must stay valid"
        );
        // Not allowed, missing, and unknown key look the same.
        let other = auth("k1", "git-upload-pack", "bob", "other");
        let missing = auth("k1", "git-upload-pack", "bob", "nope");
        let unknown = auth("k2", "git-upload-pack", "alice", "demo");
        assert!(!other.allow);
        assert_eq!(other, missing);
        assert_eq!(other, unknown);
        assert!(!auth("k1", "git-upload-archive", "alice", "demo").allow);
    }
}
