#!/bin/sh
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Runs once, when the postgres volume is created (the image's
# /docker-entrypoint-initdb.d). Slath gets a role that only owns its
# database, never the superuser: a bug in Slath, or a PostgreSQL flaw that
# needs a privileged role, then can't take over the database server. The
# superuser `postgres` stays reachable only through the container's local
# socket (`docker compose exec postgres psql -U postgres`).
set -eu

psql -v ON_ERROR_STOP=1 -v pw="$SLATH_DB_PASSWORD" --username postgres --dbname postgres <<'SQL'
CREATE ROLE slath LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION PASSWORD :'pw';
CREATE DATABASE slath OWNER slath;
SQL
# The extensions Slath's migrations create (citext, pg_trgm) are trusted:
# the database's owner creates them itself and owns them.
# Refuse network logins as the superuser (the first matching line wins).
sed -i '1i host all postgres all reject' "$PGDATA/pg_hba.conf"
