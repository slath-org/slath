<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Accounts and sign-in

## Registration

`[auth] registration` decides who can create accounts:

- `disabled` (default): only administrators, with `slath admin
  create-user` or under **Administration → Users**. `/-/register` doesn't
  exist.
- `invite`: people with an invitation link. Administrators create links
  under **Administration → Invitations**; a link works once, for 7 days, and
  can be revoked. A link bound to an email address requires that address,
  which then counts as confirmed (and, with email configured, the link is
  sent to it).
- `open`: anyone can register on the sign-in page. With email configured,
  new accounts must confirm their address before they can create
  repositories or organizations.

Registrations are limited to 5 per IP address per 15 minutes.

## Email addresses and password resets

With [email](email.md) configured, each new account's address gets a
confirmation link (valid for 48 hours). Users change their address under
**Settings → Profile**; the new address is unconfirmed until its link is
used, and the old address is told about the change.

**Forgot your password?** on the sign-in page sends a reset link to a
confirmed address. The answer is the same whether or not an account has the
address. A link is valid for 30 minutes and works once; at most three are
sent per account and hour. Setting a new password signs out every session
of the account. Accounts with two-factor authentication must also enter a
code (or a recovery code) to reset the password.

Without email, the sign-in page asks users to contact an administrator.

## Signing in with OpenID Connect

People can sign in with an OpenID Connect provider (Keycloak, Dex,
Authentik, Google Workspace, Microsoft Entra ID, …). Register Slath as a
confidential client with the redirect URI
`<public_url>/-/login/oidc/<key>/callback`, then add the provider:

```toml
[[auth.oidc]]
key = "company"                 # used in URLs
display_name = "Company login"  # shown on the sign-in button
issuer = "https://id.example.com/realms/main"   # must equal the tokens' iss
client_id = "slath"
client_secret_file = "/etc/slath/oidc-company.secret"
# scopes = ["openid", "email", "profile"]
# allow_registration = false    # create accounts at first sign-in
# trust_email = false           # see below
# counts_as_mfa = false         # sign-ins satisfy second-factor requirements
```

- Slath uses the Authorization Code flow with PKCE, a single-use `state`
  that expires after ten minutes and only works in the browser that
  started the sign-in, and a `nonce`. ID tokens must be signed with RS256,
  ES256, or EdDSA by a key from the provider's key set (never `none`), and
  `iss`, `aud`, `azp`, `exp`, `iat`, and `nonce` are checked. Discovery and
  keys are fetched from the issuer and kept for a day; the provider must
  use HTTPS (plain HTTP is accepted only for a provider on the same
  machine, for testing).
- An account is linked to a provider identity by issuer and subject, never
  by email. People link a provider under **Settings → Security** after
  confirming their password; unlinking needs the same.
- At the first sign-in with an unknown identity, `allow_registration`
  lets the person create an account by choosing a username. If the
  provider's verified email belongs to an existing account and
  `trust_email` is on, Slath doesn't link it; it tells the person to sign
  in to that account and link the provider there.
- Accounts with two-factor authentication still enter a code after an
  OIDC sign-in unless the provider has `counts_as_mfa = true`. Suspended
  accounts can't sign in this way either.
- Profile pictures from the provider are never loaded.

## Two-factor authentication

Users turn on two-factor authentication under **Settings → Security**: they
scan a QR code with an authenticator app (or type the setup key), confirm
with a code, and get 10 single-use recovery codes. From then on, signing in
and confirming sensitive actions need a 6-digit code (or a recovery code)
besides the password.

- A code works once. Codes from the previous and next 30-second step are
  accepted, for clocks that are slightly off.
- Five wrong codes end the sign-in attempt; failures also count toward the
  account's sign-in limit.
- Turning two-factor authentication off needs the password and a code.
  Users get an email when it's turned on or off and when a recovery code is
  used to sign in (if email is configured).

### Passkeys

Under **Settings → Security**, users add passkeys: the device's screen lock
(fingerprint, face, PIN) or a security key. A passkey signs in on its own
(**Sign in with a passkey**), works as the second step after the password,
and confirms sensitive actions. Every passkey must verify the user, so it
counts as two-factor authentication.

Passkeys need `server.public_url` to use a host name (not an IP address)
and, except for `localhost`, HTTPS. Without that, the passkey options are
hidden. The relying party ID is the host of `public_url`, and only that
exact origin is accepted.

### Administrators

With `[auth] require_mfa_for_admins = true` (the default), administrator
powers apply only when the administrator signed in with a second factor,
and to tokens and SSH keys only if the account has one. Until then, an
administrator is treated like a regular user, and the pages show a notice
explaining what to do.

### Lost devices

A user who lost their authenticator signs in with a recovery code. Without
one, an administrator removes their second factors:

```sh
slath admin reset-mfa --username alice
```

This also signs the user out everywhere. It's recorded in the audit log, and
the user gets an email. They then sign in with their password and set up
two-factor authentication again.

## Organizations and teams

A user's role on a repository is the highest of everything that grants
one: being a collaborator, being in a team with access, owning the
organization (admin), every member's default access (none or read),
owning the namespace (admin), and site administration (admin). Removing
any grant takes effect on the next request.

- **Teams:** organization owners create teams under the organization's
  **Teams** page and add organization members. A repository's
  administrators give a team of the same organization a role under
  **Settings → Access**. Leaving the organization also removes a user
  from its teams; deleting a team ends the access it gave.
- **Member policies** (organization **Settings**): the access every member
  gets (none, or read for every repository), and **Require two-factor
  authentication**. With the requirement, members and collaborators
  without a second factor lose access to the organization's repositories
  until they set one up (public repositories stay readable).

## Bots

Bots are accounts for automation and AI agents, owned by a person or an
organization ([Bots](../user/bots.md)). They never sign in and work only
through the tokens and SSH keys their owner gives them. These settings
apply to all of them:

```toml
[auth]
bot_token_default_days = 30   # validity of a new bot token
bot_token_max_days = 90       # the longest validity an owner can choose

[limits]
bot_requests_per_hour = 2000  # API requests per bot and hour
```

To create a bot and a token on the server, for example while setting up an
instance for agents:

```sh
slath admin create-bot --owner alice --name alice-ci \
  --token-scopes repo:write,mrs:write --repo alice/app --output json
```

The token is printed once. The owner can see and revoke it later.

To set up several accounts, bots, repositories, and tokens at once, and
repeatably, use a bootstrap file with `slath admin apply`
([Setting up an instance from a file](bootstrap.md)).

Suspending a person under **Administration → Users** also suspends their
bots; reactivating the person reactivates those bots. Bots suspended on
their own stay suspended, and can't be reactivated while their owner is
suspended. Each suspension's audit entry counts the bots it affected.

`slath admin bot stop NAME` stops a bot at once, without signing in: it's
suspended, its tokens are revoked, its SSH keys removed, and its pushes
refused. `slath admin bot start NAME` re-enables a stopped bot without
credentials. Owners and organization owners can do the same through the
API (see [Bots](../user/bots.md#stopping-a-bot)); reactivating a bot under
**Administration → Users** also ends its stop.

To pause every bot of the instance at once, freeze them:
`slath admin bot freeze` (or `PUT /api/v1/admin/bot-freeze` with a token
that has the `admin` scope). Frozen bots can't use their tokens or SSH
keys and their pushes are refused; nothing is revoked, and
`slath admin bot unfreeze` restores access. `--org ORG` freezes only an
organization's bots, as its owners can through the API.
