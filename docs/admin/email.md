<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Email

Slath sends email for account verification, password resets, and
notifications. Without an SMTP relay, these features are hidden and an
administrator resets passwords instead.

```toml
[smtp]
host = "smtp.example.org"
port = 587
tls = "starttls"                       # starttls (587) | tls (465) | none
username = "slath"
password_file = "/etc/slath/smtp-password"
from = "Slath <noreply@example.org>"
```

- `tls = "starttls"` requires STARTTLS; Slath doesn't fall back to an
  unencrypted connection. Use `none` only for a relay on the same host.
- The password file should be readable only by the `slath` user.

Every message is queued and sent by a background job, which retries
temporary failures with increasing delays (up to 8 attempts). Permanent
failures, such as an unknown recipient, aren't retried. Failed messages
appear as dead jobs under **Administration → Health**.

Check the settings with:

```sh
slath admin send-test-email --to you@example.org
```

or with **Send test message** under **Administration → Health**.

Queued messages are stored encrypted with the master key until they are
sent, because they can contain single-use links.
