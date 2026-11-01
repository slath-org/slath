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
