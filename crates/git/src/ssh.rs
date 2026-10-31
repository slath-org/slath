// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parsing the command an SSH client asked for (`SSH_ORIGINAL_COMMAND`).
//!
//! Only `git-upload-pack`, `git-receive-pack`, and `git-upload-archive`
//! (and their `git <service>` spellings) with one single-quoted
//! `<namespace>/<repo>[.git]` path are recognized. The path is never
//! unquoted shell-style: anything other than plain name characters is
//! rejected, so traversal (`..`), extra arguments, and shell syntax can't
//! get through.

use slath_core::names::RepoName;

/// What the client asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCommand {
    pub service: SshService,
    pub namespace: String,
    pub repo: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshService {
    UploadPack,
    ReceivePack,
    /// Recognized so it can be refused with a clear message.
    UploadArchive,
}

impl SshService {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
            Self::UploadArchive => "git-upload-archive",
        }
    }
}

/// Parses `SSH_ORIGINAL_COMMAND`. `None` means "unsupported command".
#[must_use]
pub fn parse_command(cmd: &str) -> Option<SshCommand> {
    let (service, rest) = [
        ("git-upload-pack ", SshService::UploadPack),
        ("git upload-pack ", SshService::UploadPack),
        ("git-receive-pack ", SshService::ReceivePack),
        ("git receive-pack ", SshService::ReceivePack),
        ("git-upload-archive ", SshService::UploadArchive),
        ("git upload-archive ", SshService::UploadArchive),
    ]
    .iter()
    .find_map(|(prefix, svc)| cmd.strip_prefix(prefix).map(|rest| (*svc, rest)))?;
    let path = rest.strip_prefix('\'')?.strip_suffix('\'')?;
    let path = path.strip_prefix('/').unwrap_or(path);
    let (namespace, repo) = path.split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    // Same character rules as names: rejects '..', quotes, slashes, spaces.
    RepoName::parse(namespace).ok()?;
    RepoName::parse(repo).ok()?;
    Some(SshCommand {
        service,
        namespace: namespace.to_owned(),
        repo: repo.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_git_client_commands() {
        let c = parse_command("git-upload-pack 'alice/demo.git'").unwrap();
        assert_eq!(
            (c.service, c.namespace.as_str(), c.repo.as_str()),
            (SshService::UploadPack, "alice", "demo")
        );
        assert_eq!(
            parse_command("git-receive-pack '/alice/demo'")
                .unwrap()
                .service,
            SshService::ReceivePack
        );
        assert_eq!(
            parse_command("git upload-pack 'alice/demo'").unwrap().repo,
            "demo"
        );
        assert_eq!(
            parse_command("git-upload-archive 'alice/demo.git'")
                .unwrap()
                .service,
            SshService::UploadArchive
        );
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "ls",
            "sh -i",
            "git-upload-pack",
            "git-upload-pack alice/demo",
            "git-upload-pack 'alice/demo",
            "git-upload-pack '../etc/passwd'",
            "git-upload-pack 'alice/../bob/x'",
            "git-upload-pack 'alice/demo/extra'",
            "git-upload-pack '/srv/git/alice/demo'",
            "git-upload-pack 'alice/demo'; rm -rf /",
            "git-upload-pack 'alice/demo' --upload-pack=x",
            "git-upload-pack 'alice/.git'",
            "git-upload-pack 'alice/-x'",
            "git-upload-pack 'al ice/demo'",
            "git-upload-pack 'alice/de'mo'",
            "git-upload-pack \"alice/demo\"",
            "git-upload-pack 'alice/demo'\n",
            "git-shell -c 'git-upload-pack alice/demo'",
            "scp -t /tmp",
        ] {
            assert_eq!(parse_command(bad), None, "{bad:?}");
        }
    }
}
