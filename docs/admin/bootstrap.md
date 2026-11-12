<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Setting up an instance from a file

`slath admin apply FILE` creates users, organizations, bots, repositories,
and tokens from a TOML file. It's meant for scripted setups: a test or
demo instance, an instance for AI agents, or the first accounts of a new
installation.

```sh
slath admin apply seed.toml
slath admin apply seed.toml --output json
```

Applying is **idempotent**: whatever already exists is left as it is, and
only what's missing is created, so running the same file again reports
`0 changes`. Nothing is ever deleted or renamed. If an entry can't be
applied, `apply` stops with an error; what it applied before that stays,
and running it again after fixing the file picks up from there.

Each change is recorded in the audit log, as an action of the account
that owns what was created (a repository by its namespace's owner), plus
one `admin.apply` entry.

## The file

```toml
[[users]]
username = "ada"
email = "ada@example.org"
admin = true                          # site administrator (default false)
password_file = "secrets/ada-password"

[[orgs]]
name = "acme"
owners = ["ada"]                      # at least one; the first creates it
members = []

[[bots]]
name = "acme-agent"
owner = "acme"                        # a user or an organization

[[repos]]
namespace = "acme"
name = "app"
visibility = "private"                # public, internal, or private (default)
default_branch = "main"               # default main
collaborators = { "acme-agent" = "write" }

[[tokens]]
name = "agent"
bot = "acme-agent"                    # or user = "ada"
scopes = ["repo:write", "mrs:write"]
repos = ["acme/app"]                  # required for bots
days = 30                             # optional
file = "secrets/bot-token"
```

Paths are relative to the file. Unknown keys are errors.

- **Users** are created once. With `password_file`, the password is read
  from that file if it exists; otherwise one is generated and written
  there. Without `password_file`, a generated password is printed once.
- **Organizations** get the listed owners and members that aren't in
  them yet. People already in an organization keep their role.
- **Bots** are created for their owner. A bot of the same name with
  another owner is an error.
- **Repositories** are created by their namespace's owner (for an
  organization, its first owner). Listed collaborators get the given role,
  also when they had another one.
- **Tokens** follow the rules for [bot tokens](../user/bots.md#tokens) or
  personal tokens (people's tokens never expire unless `days` is given,
  and only site administrators get `admin`). A token is created when its
  account has no active token of that name, or when its `file` is
  missing; the token it replaces is revoked.

Passwords and tokens are written only to their files, with mode 0600
(directories 0700), and never appear in the output. Keep these files
somewhere only the people and agents who need them can read.
