// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hardened Git runner.
//!
//! This is the **only** place in the workspace that spawns Git. Every process:
//!
//! 1. runs the configured absolute binary;
//! 2. gets a scrubbed, fixed environment;
//! 3. gets `--git-dir=<abs path>` and cwd `<data_dir>/git-home`;
//! 4. gets `-c core.hooksPath=…`, `-c core.fsmonitor=false`, `-c protocol.allow=never`;
//! 5. runs in its own process group that is killed on timeout, output overflow, or drop;
//! 6. holds a permit of the global concurrency semaphore while it runs;
//! 7. has its stderr captured (capped) for logs only.

use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::{GitError, LimitKind};
use crate::version::{GitVersion, MIN_GIT_VERSION};

/// Maximum stderr kept per process.
pub const STDERR_CAP: usize = 64 * 1024;

/// `PATH` given to every Git process.
const CHILD_PATH: &str = "/usr/bin:/bin";

/// Static configuration of the runner, taken from `[git]`, `[storage]`, and `[limits]`.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Absolute path of the Git binary.
    pub git_bin: PathBuf,
    /// Empty directory used as `HOME` and cwd (`<data_dir>/git-home`).
    pub home_dir: PathBuf,
    /// Directory with the managed `pre-receive`/`post-receive` hooks.
    pub hooks_dir: Option<PathBuf>,
    /// `limits.git_max_concurrent`.
    pub max_concurrent: usize,
    /// `limits.git_read_timeout`: default timeout for [`Invocation::output`].
    pub read_timeout: Duration,
    /// Default cap on captured stdout.
    pub max_output_bytes: usize,
}

impl RunnerConfig {
    /// Default limits.
    #[must_use]
    pub fn new(git_bin: PathBuf, home_dir: PathBuf) -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, usize::from);
        Self {
            git_bin,
            home_dir,
            hooks_dir: None,
            max_concurrent: 2 * cpus,
            read_timeout: Duration::from_secs(30),
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Spawns Git processes under the rules in the module docs. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Runner {
    cfg: Arc<RunnerConfig>,
    sem: Arc<Semaphore>,
}

/// What a process is for; selects the hooks and protocol configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Reads and internal writes: hooks disabled.
    Default,
    /// `receive-pack` for client pushes: managed hooks enabled.
    ReceivePack,
}

impl Runner {
    pub fn new(cfg: RunnerConfig) -> Result<Self, GitError> {
        if !cfg.git_bin.is_absolute() || !cfg.home_dir.is_absolute() {
            return Err(GitError::InvalidArgument(
                "git binary and home must be absolute paths",
            ));
        }
        let permits = cfg.max_concurrent.max(1);
        Ok(Self {
            cfg: Arc::new(cfg),
            sem: Arc::new(Semaphore::new(permits)),
        })
    }

    #[must_use]
    pub fn config(&self) -> &RunnerConfig {
        &self.cfg
    }

    /// A Git command without a repository (e.g. `git --version`).
    pub fn global(&self) -> Invocation {
        Invocation::new(self.clone(), None, Mode::Default)
    }

    /// A Git command on the repository at `git_dir` (absolute, derived from an ID).
    pub fn repo(&self, git_dir: &Path) -> Invocation {
        Invocation::new(self.clone(), Some(git_dir.to_path_buf()), Mode::Default)
    }

    /// `receive-pack` for a client push, with the managed hooks enabled.
    pub fn receive_pack(&self, git_dir: &Path) -> Invocation {
        Invocation::new(self.clone(), Some(git_dir.to_path_buf()), Mode::ReceivePack)
    }

    /// Returns the Git version, failing if it's older than [`MIN_GIT_VERSION`].
    pub async fn check_version(&self) -> Result<GitVersion, GitError> {
        let out = self.global().arg("--version").run().await?;
        let text = String::from_utf8_lossy(&out.stdout);
        let version =
            GitVersion::parse(&text).ok_or(GitError::Protocol("unparseable git --version"))?;
        if version < MIN_GIT_VERSION {
            return Err(GitError::UnsupportedVersion {
                found: version.to_string(),
                required: MIN_GIT_VERSION.to_string(),
            });
        }
        Ok(version)
    }
}

/// Collected output of a finished process.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// For logs only; never send to clients.
    pub stderr: Vec<u8>,
}

/// A Git command being built. Arguments are added by Slath code only; user
/// values must come from the validated types in [`crate::args`] and go after
/// `--end-of-options` or `--`.
#[must_use]
pub struct Invocation {
    runner: Runner,
    git_dir: Option<PathBuf>,
    mode: Mode,
    args: Vec<OsString>,
    env: Vec<(&'static str, OsString)>,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
}

impl Invocation {
    fn new(runner: Runner, git_dir: Option<PathBuf>, mode: Mode) -> Self {
        let timeout = runner.cfg.read_timeout;
        let max_output = runner.cfg.max_output_bytes;
        Self {
            runner,
            git_dir,
            mode,
            args: Vec::new(),
            env: Vec::new(),
            stdin: None,
            timeout,
            max_output,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    /// Adds a Git config override (`-c key=value`) for this invocation.
    /// Only for values Slath computes itself, never user input.
    pub fn config(self, key: &str, value: impl std::fmt::Display) -> Self {
        self.arg("-c").arg(format!("{key}={value}"))
    }

    /// Adds a byte-string argument (ref names and paths may be non-UTF-8).
    pub fn arg_bytes(self, arg: &[u8]) -> Self {
        self.arg(OsStr::from_bytes(arg))
    }

    pub fn stdin(mut self, data: Vec<u8>) -> Self {
        self.stdin = Some(data);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_output(mut self, bytes: usize) -> Self {
        self.max_output = bytes;
        self
    }

    /// Passes the client's `GIT_PROTOCOL` value through if it is well-formed
    /// (`^version=[12](:[a-z0-9=_-]+)*$`); anything else is dropped.
    pub fn git_protocol(mut self, value: &str) -> Self {
        if is_valid_git_protocol(value) {
            self.env.push(("GIT_PROTOCOL", value.into()));
        }
        self
    }

    /// Runs with the objects of a push that is still in quarantine (during
    /// pre-receive): `object_dir` is the quarantine directory, which must
    /// lie inside `git_dir`'s `objects` directory; the repository's own
    /// objects are its only alternate. Anything else is refused, because the
    /// paths come from the hook (via IPC).
    pub fn quarantine(mut self, git_dir: &Path, object_dir: &Path) -> Result<Self, GitError> {
        let objects = std::fs::canonicalize(git_dir.join("objects"))?;
        let quarantine = std::fs::canonicalize(object_dir)?;
        if quarantine.parent() != Some(objects.as_path())
            || !quarantine
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("tmp_objdir-incoming-"))
        {
            return Err(GitError::InvalidArgument(
                "quarantine directory outside the repository",
            ));
        }
        self.env
            .push(("GIT_OBJECT_DIRECTORY", quarantine.into_os_string()));
        self.env
            .push(("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects.into_os_string()));
        Ok(self)
    }

    /// Sets the variables `slath hook` needs to reach the server.
    pub fn hook_env(mut self, token: &str, ipc_socket: &Path) -> Self {
        self.env.push(("SLATH_HOOK_TOKEN", token.into()));
        self.env
            .push(("SLATH_IPC", ipc_socket.as_os_str().to_owned()));
        self
    }

    fn command(&self) -> Command {
        let cfg = &self.runner.cfg;
        let mut cmd = Command::new(&cfg.git_bin);
        cmd.env_clear()
            .env("PATH", CHILD_PATH)
            .env("HOME", &cfg.home_dir)
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_ATTR_NOSYSTEM", "1");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.current_dir(&cfg.home_dir);
        if let Some(dir) = &self.git_dir {
            let mut a = OsString::from("--git-dir=");
            a.push(dir);
            cmd.arg(a);
        }
        let hooks = match (self.mode, &cfg.hooks_dir) {
            (Mode::ReceivePack, Some(dir)) => dir.as_os_str().to_owned(),
            _ => OsString::from("/dev/null"),
        };
        let mut hooks_arg = OsString::from("core.hooksPath=");
        hooks_arg.push(hooks);
        cmd.arg("-c").arg(hooks_arg);
        cmd.args(["-c", "core.fsmonitor=false", "-c", "protocol.allow=never"]);
        cmd.args(&self.args);
        cmd.process_group(0).kill_on_drop(true);
        cmd
    }

    /// Spawns the process with piped stdio for streaming use (transport,
    /// long-lived `cat-file`). The caller enforces its own I/O timeouts; the
    /// process group is killed when the returned [`GitChild`] is dropped.
    pub async fn spawn(self) -> Result<GitChild, GitError> {
        let permit = Arc::clone(&self.runner.sem)
            .acquire_owned()
            .await
            .map_err(|_| GitError::Closed)?;
        let mut cmd = self.command();
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn()?;
        let group = ProcessGroup::new(&child);
        Ok(GitChild {
            child,
            group,
            _permit: permit,
        })
    }

    /// Spawns the process attached to the given stdin, stdout, and stderr
    /// (used for SSH, where they are the SSH channel passed in by
    /// `ssh-serve`). This process keeps no copies of them.
    pub async fn spawn_attached(self, stdio: [OwnedFd; 3]) -> Result<GitChild, GitError> {
        let permit = Arc::clone(&self.runner.sem)
            .acquire_owned()
            .await
            .map_err(|_| GitError::Closed)?;
        let mut cmd = self.command();
        let [stdin, stdout, stderr] = stdio;
        cmd.stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let child = cmd.spawn()?;
        drop(cmd);
        let group = ProcessGroup::new(&child);
        Ok(GitChild {
            child,
            group,
            _permit: permit,
        })
    }

    /// Runs to completion and returns the output whatever the exit status.
    /// Exceeding the timeout or the output cap kills the process group and
    /// returns [`GitError::Limit`].
    pub async fn output(mut self) -> Result<Output, GitError> {
        let timeout = self.timeout;
        let max_output = self.max_output;
        let input = self.stdin.take();
        let mut child = self.spawn().await?;
        let (stdin, stdout, stderr) = child.take_stdio()?;

        let io = async {
            let write = async move {
                let mut stdin = stdin;
                if let Some(data) = input {
                    // The child may exit without reading everything; that's
                    // reported through its exit status, not as a write error.
                    let _ = stdin.write_all(&data).await;
                }
                drop(stdin);
                Ok::<(), GitError>(())
            };
            let (_, out, err) =
                tokio::try_join!(write, read_capped(stdout, max_output), read_stderr(stderr))?;
            let status = child.child.wait().await?;
            child.group.reaped();
            Ok::<_, GitError>(Output {
                status,
                stdout: out,
                stderr: err,
            })
        };
        match tokio::time::timeout(timeout, io).await {
            Ok(result) => result,
            Err(_) => Err(GitError::Limit(LimitKind::Timeout)),
        }
        // On error paths `child` is dropped here, which kills the group.
    }

    /// Like [`Invocation::output`], but a non-zero exit is an error.
    pub async fn run(self) -> Result<Output, GitError> {
        let out = self.output().await?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(GitError::Failed {
                status: out.status,
                stderr: out.stderr,
            })
        }
    }
}

fn is_valid_git_protocol(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("version=") else {
        return false;
    };
    let mut parts = rest.split(':');
    if !matches!(parts.next(), Some("1" | "2")) {
        return false;
    }
    parts.all(|p| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"=_-".contains(&b))
    })
}

async fn read_capped(mut r: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>, GitError> {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(buf);
        }
        if buf.len() + n > cap {
            return Err(GitError::Limit(LimitKind::Output));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Reads all of stderr but keeps only the first [`STDERR_CAP`] bytes, so a
/// chatty process never blocks on a full pipe.
pub(crate) async fn read_stderr(mut r: impl AsyncRead + Unpin) -> Result<Vec<u8>, GitError> {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 8 * 1024];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(buf);
        }
        let keep = n.min(STDERR_CAP.saturating_sub(buf.len()));
        buf.extend_from_slice(&chunk[..keep]);
    }
}

/// Kills the whole process group of a child unless it was reaped normally.
#[derive(Debug)]
struct ProcessGroup {
    pgid: Option<Pid>,
}

impl ProcessGroup {
    fn new(child: &Child) -> Self {
        let pgid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .map(Pid::from_raw);
        Self { pgid }
    }

    /// The leader exited and was waited for; don't signal its (possibly
    /// reused) process group ID any more.
    fn reaped(&mut self) {
        self.pgid = None;
    }

    fn kill(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            // ESRCH just means the group is already gone.
            let _ = killpg(pgid, Signal::SIGKILL);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A running Git process. Dropping it kills the process group.
#[derive(Debug)]
pub struct GitChild {
    child: Child,
    group: ProcessGroup,
    _permit: OwnedSemaphorePermit,
}

impl GitChild {
    /// Takes the piped stdin, stdout, and stderr handles.
    pub fn take_stdio(&mut self) -> Result<(ChildStdin, ChildStdout, ChildStderr), GitError> {
        match (
            self.child.stdin.take(),
            self.child.stdout.take(),
            self.child.stderr.take(),
        ) {
            (Some(i), Some(o), Some(e)) => Ok((i, o, e)),
            _ => Err(GitError::Protocol("stdio already taken")),
        }
    }

    /// Waits for the process to exit.
    pub async fn wait(&mut self) -> Result<ExitStatus, GitError> {
        let status = self.child.wait().await?;
        self.group.reaped();
        Ok(status)
    }

    /// Kills the process group immediately.
    pub fn kill(&mut self) {
        self.group.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_protocol_values() {
        for ok in [
            "version=2",
            "version=1",
            "version=2:object-format=sha1",
            "version=2:a:b_c",
        ] {
            assert!(is_valid_git_protocol(ok), "{ok}");
        }
        for bad in [
            "",
            "version=3",
            "version=2:",
            "version=2::x",
            "version=2:A",
            "version=2\n",
            "x=1",
        ] {
            assert!(!is_valid_git_protocol(bad), "{bad:?}");
        }
    }
}
