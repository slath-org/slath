# syntax=docker/dockerfile:1
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# A clean Debian host for the `slath setup` conformance test
# (scripts/test-setup.sh): PostgreSQL and OpenSSH from the distribution, a
# current Git, and the downloaded `slath` binary in /root. Nothing of Slath
# is set up. systemd doesn't run in it, so setup leaves starting to the test.

ARG SLATH_IMAGE=slath:test
FROM ${SLATH_IMAGE} AS slath

FROM docker.io/library/debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        postgresql openssh-server systemd-standalone-sysusers \
        ca-certificates zlib1g libcurl4t64 libexpat1 \
    && rm -rf /var/lib/apt/lists/*
# Before 1.0, Slath needs the newest Git; the distribution's is older.
COPY --from=slath /opt/git /opt/git
RUN ln -s /opt/git/bin/git /usr/local/bin/git
COPY --from=slath /usr/bin/slath /root/slath
CMD ["sleep", "infinity"]
