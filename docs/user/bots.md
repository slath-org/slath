<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Bots

A bot is an account for automation: a CI system, a release script, or an
AI coding agent. It belongs to a person or an organization, acts only
through the credentials its owner gives it, and is marked as a bot
wherever it appears, so nobody mistakes its work for a person's.

## Creating a bot

Anyone can create bots of their own. An organization's bots are created
by the organization's owners.

Through the API, with a token that has the `user:write` scope:

```sh
# A bot of your own
curl -X POST https://git.example.org/api/v1/user/bots \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"username": "alice-ci", "display_name": "Alice'\''s CI"}'

# A bot of the organization acme (organization owners)
curl -X POST https://git.example.org/api/v1/orgs/acme/bots \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"username": "acme-release"}'
```

A bot's name follows the same rules as usernames and shares their
namespace: it can't be the name of a user, an organization, or another
bot. `GET /api/v1/user/bots`, `GET /api/v1/orgs/{org}/bots`, and
`GET /api/v1/bots/{name}` list and show the bots you manage. Other
people's bots don't appear there.

## What a bot can't do

- **Sign in.** A bot has no password, no session, no second factor, and
  no OpenID Connect link. The sign-in page refuses its name exactly as it
  refuses an unknown one.
- **Own organizations or create bots.** Bots can be members of an
  organization but never its owners, and they can't create organizations
  or bots of their own.
