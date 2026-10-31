# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# sshd test fixture for the SSH compatibility tests (built by tests/common/sshd.rs).
# Arch Linux: rolling, so it carries current OpenSSH, Git, and a glibc at least
# as new as the one the test binary was built against. (The git package ships
# a `git` system user, which is replaced by one with the test runner's UID.)
FROM docker.io/library/archlinux:latest

ARG UID=1000
RUN pacman -Syu --noconfirm --needed openssh git \
    && pacman -Scc --noconfirm \
    && (userdel git 2>/dev/null || true) \
    && useradd --uid "$UID" --create-home --shell /bin/bash git \
    && useradd --non-unique --uid "$UID" --no-create-home --shell /usr/bin/nologin slath \
    && usermod -p '*' git
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
