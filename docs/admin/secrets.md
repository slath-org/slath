<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->
# Secrets and the master key

Slath stores passwords and access tokens only as hashes. Secrets it must be
able to read again (two-factor seeds, and later webhook and CI secrets) are
encrypted with AES-256-GCM under a key derived from the **master key**
(`secrets.master_key_file`). A database dump on its own contains no
readable secret.

Each encrypted value records the ID of the master key that sealed it. The
key itself never enters the database or a backup: back up the key file
separately (see [backup](backup.md)).

## Rotating the master key

1. Generate a new key next to the old one:

   ```sh
   slath admin gen-key --output /etc/slath/master.key.new
   ```

2. In the configuration, point `secrets.master_key_file` at the new file and
   list the old one under `secrets.old_key_files`:

   ```toml
   [secrets]
   master_key_file = "/etc/slath/master.key.new"
   old_key_files = ["/etc/slath/master.key"]
   ```

3. Restart `slath serve`. New secrets are now sealed with the new key, and
   existing ones are still readable with the old key.

4. Re-encrypt everything:

   ```sh
   slath admin rotate-key
   ```

   The command re-encrypts value by value. It is safe to interrupt and run
   again; values already under the new key are skipped. When every value
   uses the new key, it marks the old key as retired and says so.

5. Remove the old file from `secrets.old_key_files`, restart, and destroy
   the old key once no backup you might restore still needs it: restoring
   an older backup brings back values sealed with the old key, which then
   need it again.

If a value can't be decrypted (for example because an old key file is
missing), `rotate-key` names the row, exits with status 1, and retires
nothing.

Rotating the master key also signs the CSRF tokens of open pages with a new
key, so forms opened before the restart have to be reloaded.
