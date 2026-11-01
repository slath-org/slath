# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# sshd test fixture for the SSH compatibility tests (built by tests/common/sshd.rs).
# Arch Linux: rolling, so it carries current OpenSSH and a glibc at least as
# new as the one the test binary was built against.
#
# As in a real install, the `git` login is a user of its own (GIT_UID, not the
# test server's UID) whose only link to the server is group GID, the IPC
# socket's group. It gets no data directory: Git runs in the server. `slath`
# (the server's UID) exists only so that permission tests can name it.
FROM docker.io/library/archlinux:latest

ARG UID=1000
ARG GID=1000
ARG GIT_UID=10002
RUN pacman -Syu --noconfirm --needed openssh \
    && pacman -Scc --noconfirm \
    && (userdel git 2>/dev/null || true) \
    && (groupdel git 2>/dev/null || true) \
    && groupadd --non-unique --gid "$GID" slath-ipc \
    && useradd --non-unique --uid "$UID" --gid "$GID" --no-create-home \
        --shell /usr/bin/nologin slath \
    && useradd --uid "$GIT_UID" --user-group --groups slath-ipc --home-dir / \
        --no-create-home --shell /bin/sh git \
    && usermod -p '*' git
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
