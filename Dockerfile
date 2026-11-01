# syntax=docker/dockerfile:1
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Release image: the slath binary (UI assets embedded), a pinned Git built from
# source, and OpenSSH. The same image runs `slath serve` (default) and sshd
# (`/usr/local/lib/slath/sshd-entrypoint`, see deploy/compose/compose.yml).
#
#   docker build -t slath .

ARG DEBIAN=trixie
# Before 1.0, Slath requires the newest stable Git (tests/compat/versions.toml).
ARG GIT_VERSION=2.56.0

# --- UI bundle ----------------------------------------------------------------
FROM docker.io/library/node:24-${DEBIAN}-slim AS web
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY scripts/build_icons.py scripts/
COPY web/ web/
RUN cd web && npm ci --no-audit --no-fund && npm run build

# --- Git (its build needs cargo, too) -----------------------------------------
FROM docker.io/library/rust:1.99-slim-${DEBIAN} AS git
ARG GIT_VERSION
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        make ca-certificates curl python3 xz-utils \
        zlib1g-dev libcurl4-openssl-dev libexpat1-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY scripts/install-git.sh scripts/pinned_sha256.py scripts/
COPY tests/compat/versions.toml tests/compat/
RUN scripts/install-git.sh "$GIT_VERSION" /opt/git \
    && rm -rf /opt/git/share/man /opt/git/share/doc

# --- slath ----------------------------------------------------------------------
FROM docker.io/library/rust:1.99-slim-${DEBIAN} AS build
WORKDIR /src
COPY . .
COPY --from=web /src/web/dist web/dist
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p slath --bin slath \
    && install -D -m 0755 target/release/slath /out/slath

# --- runtime --------------------------------------------------------------------
FROM docker.io/library/debian:${DEBIAN}-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates openssh-server tini zlib1g libcurl4t64 libexpat1 \
    && rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
    # `slath` runs the server and owns the data. `git` is the SSH login, a
    # user of its own: its group may connect to the IPC socket in /run/slath
    # (slath is a member, to hand the socket to it), and that is all.
    && groupadd --gid 10001 slath \
    && useradd --uid 10001 --gid slath --home-dir /var/lib/slath --no-create-home \
        --shell /usr/sbin/nologin slath \
    && groupadd --gid 10002 git \
    && useradd --uid 10002 --gid git --home-dir / --no-create-home --shell /bin/sh git \
    && usermod --password '*' git \
    && usermod --append --groups git slath \
    && install -d -o slath -g slath -m 0750 /var/lib/slath \
    && install -d -o slath -g git -m 0710 /run/slath \
    && install -d -m 0755 /etc/slath /run/sshd
COPY --from=git /opt/git /opt/git
COPY --from=build /out/slath /usr/bin/slath
COPY deploy/docker/slath.toml /etc/slath/slath.toml
COPY deploy/sshd/slath.conf /etc/ssh/sshd_config.d/slath.conf
COPY deploy/docker/sshd-entrypoint.sh /usr/local/lib/slath/sshd-entrypoint
COPY LICENSES/ /usr/share/doc/slath/LICENSES/
COPY THIRD-PARTY-NOTICES.md /usr/share/doc/slath/
# Set here, not in slath.toml, so a mounted configuration keeps it.
ENV SLATH_CONFIG=/etc/slath/slath.toml SLATH__STORAGE__SOCKET_GROUP=git
VOLUME ["/var/lib/slath", "/run/slath"]
EXPOSE 3000 22
USER slath
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/bin/slath"]
CMD ["serve", "--migrate"]
