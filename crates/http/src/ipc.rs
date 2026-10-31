// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The IPC server that `slath hook` talks to.
//!
//! The socket lives in the `run_dir` (mode 0700) and is itself mode 0600.
//! Each transport request mints a hook token bound to its repository; hooks
//! must present it, so a token can't be used for another repository or
//! after the request ends.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slath_git::args::RefName;
use slath_git::hookipc::{MAX_MESSAGE, RefUpdate, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

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
    registry: Arc<HookRegistry>,
}

impl HookToken {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.token
    }
}

impl Drop for HookToken {
    fn drop(&mut self) {
        if let Ok(mut g) = self.registry.grants.lock() {
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
            registry: Arc::clone(self),
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
pub async fn serve(listener: UnixListener, registry: Arc<HookRegistry>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let registry = Arc::clone(&registry);
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, &registry).await {
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

async fn handle_conn(stream: UnixStream, registry: &HookRegistry) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read.take(MAX_MESSAGE as u64));
    let mut line = Vec::new();
    let read_line = reader.read_until(b'\n', &mut line);
    tokio::time::timeout(Duration::from_secs(10), read_line)
        .await
        .map_err(|_| std::io::Error::other("ipc read timeout"))??;
    let response = match serde_json::from_slice::<Request>(&line) {
        Ok(req) => handle(req, registry),
        Err(_) => deny("slath: malformed hook request"),
    };
    let mut out = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
    out.push(b'\n');
    write.write_all(&out).await
}

fn deny(msg: &str) -> Response {
    Response {
        allow: false,
        messages: vec![msg.to_owned()],
    }
}

fn handle(req: Request, registry: &HookRegistry) -> Response {
    match req {
        Request::PreReceive {
            token,
            updates,
            quarantine: _,
        } => {
            let Some(grant) = registry.lookup(&token) else {
                return deny("slath: push rejected: invalid or expired hook token");
            };
            pre_receive(&grant, &updates)
        }
        Request::PostReceive { token, updates } => {
            let Some(grant) = registry.lookup(&token) else {
                return deny("slath: invalid or expired hook token");
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
            Response {
                allow: true,
                messages: Vec::new(),
            }
        }
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
        Response {
            allow: true,
            messages,
        }
    } else {
        messages.push("slath: push rejected".to_owned());
        Response {
            allow: false,
            messages,
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

    #[test]
    fn reserved_refs_are_denied_atomically() {
        let reg = Arc::new(HookRegistry::default());
        let t = reg.mint("/x".into(), Duration::from_secs(60)).unwrap();
        let req = |names: &[&str]| Request::PreReceive {
            token: t.as_str().to_owned(),
            updates: names.iter().map(|n| update(n)).collect(),
            quarantine: Default::default(),
        };
        assert!(handle(req(&["refs/heads/main", "refs/tags/v1"]), &reg).allow);
        let r = handle(req(&["refs/heads/main", "refs/slath/x"]), &reg);
        assert!(!r.allow);
        assert!(r.messages[0].contains("refs/slath/x"));
        assert!(!handle(req(&["refs/merge-requests/1/head"]), &reg).allow);
    }

    #[test]
    fn tokens_are_checked_and_expire() {
        let reg = Arc::new(HookRegistry::default());
        let req = |token: &str| Request::PreReceive {
            token: token.to_owned(),
            updates: vec![update("refs/heads/main")],
            quarantine: Default::default(),
        };
        assert!(!handle(req("bogus"), &reg).allow);
        let expired = reg.mint("/x".into(), Duration::ZERO).unwrap();
        assert!(!handle(req(expired.as_str()), &reg).allow);
        let live = reg.mint("/x".into(), Duration::from_secs(60)).unwrap();
        let s = live.as_str().to_owned();
        assert_eq!(s.len(), 64);
        assert!(handle(req(&s), &reg).allow);
        drop(live);
        assert!(
            !handle(req(&s), &reg).allow,
            "token must die with its request"
        );
    }
}
