<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Webhooks

A webhook sends an HTTP request to a URL of your choice whenever something
happens in a repository: a push, a merge request or issue event, a
comment, a review, a status, a fork, or a change to the repository itself.
Use it to start a build, post to a chat, or keep another system up to
date.

## Adding a webhook

Repository administrators add webhooks under **Settings → Webhooks** of the
repository. Organization owners add them under the organization's
**Settings → Webhooks**; those receive the events of every repository of
the organization.

- **URL:** where Slath sends the events, an `https://` address. Addresses
  in internal networks (loopback, private ranges, link-local, and the
  like) are refused unless the site administrator allowed them.
- **Events:** one or more of the events below.
- **Secret:** 16 to 256 characters that you and the receiver share. Leave
  it empty and Slath generates one; it's shown once, so copy it then.
  Slath stores it encrypted and never shows it again.
- **Active:** whether it sends anything.

**Send a ping** checks that the receiver is reachable. The webhook's page
lists its deliveries from the last 30 days, each with the request, every
attempt's status and response, and **Redeliver**.

The same is available in the API under
`/api/v1/repos/{owner}/{repo}/webhooks` and `/api/v1/orgs/{org}/webhooks`.

## Events

| Event | Sent when |
|---|---|
| `push` | a branch or tag is created, updated, or deleted (`action`: `created`, `updated`, `deleted`) |
| `merge_request` | a merge request is `opened`, `updated` (a new revision), `ready`, `closed`, `reopened`, or `merged` |
| `merge_request_comment` | someone comments on a merge request or in one of its review threads |
| `merge_request_review` | someone submits a review (`approve`, `request_changes`, or `comment`) |
| `issue` | an issue is `opened`, `closed`, `reopened`, or `assigned` |
| `issue_comment` | someone comments on an issue |
| `status` | a check reports a status on a commit (`action`: the state; the `status` object includes its `annotations`) |
| `fork` | someone forks the repository |
| `repo` | the repository is `created`, `renamed`, `deleted`, or changes `visibility` |
| `ping` | you send a ping |

Every payload is JSON with `event`, `action` (where the event has one),
`repository` (`id`, `full_name`, `visibility`, `html_url`, `api_url`),
`sender` (`username`, `kind`), and the event's object: `push`,
`merge_request`, `issue`, `comment`, `review`, `status`, `fork`, or
`change`. A `push` lists at most 20 commits; very long texts are cut and
end in "… (cut)".

A webhook acts for the person who created it or last changed its URL or
secret. It sends an event only if that person can read the repository (and
its issues or merge requests) at that moment, so it never carries more than
they could see. If that account is deleted or suspended, the webhook is
switched off.

## Receiving events

Each request is a `POST` with these headers:

| Header | Value |
|---|---|
| `X-Slath-Event` | the event, e.g. `push` |
| `X-Slath-Delivery` | the delivery's ID; the same on every retry of that delivery |
| `X-Slath-Timestamp` | when this attempt was signed, in seconds since 1970 (UTC) |
| `X-Slath-Signature-256` | `sha256=` and the hex HMAC-SHA256 of the timestamp, a `.`, and the body, keyed with the secret |

Answer with any `2xx` status within 10 seconds. Anything else, a redirect,
or no answer counts as a failure: Slath tries again after 1, 2, 4, … minutes
(at most 6 hours apart), 8 attempts in all. After 50 failed attempts in a
row, Slath switches the webhook off and emails the person it acts for; turn
it on again once the receiver works. Because of retries, a receiver can see
a delivery more than once: use `X-Slath-Delivery` to ignore repeats.

### Checking the signature

Check every request before acting on it: compute the signature over the
timestamp and the raw body with your secret, compare it in constant time,
and reject timestamps older than 5 minutes so a recorded request can't be
replayed later. The samples below do exactly that; they're also available
under the Apache-2.0 license, so you can copy them into your own code.

Python:

```python
# SPDX-License-Identifier: Apache-2.0
import hashlib
import hmac
import time


def verify(secret: bytes, timestamp: str, signature: str, body: bytes) -> bool:
    """True if a Slath webhook request is genuine and recent."""
    if abs(time.time() - int(timestamp)) > 300:
        return False
    mac = hmac.new(secret, timestamp.encode() + b"." + body, hashlib.sha256)
    return hmac.compare_digest("sha256=" + mac.hexdigest(), signature)
```

JavaScript (Node.js):

```js
// SPDX-License-Identifier: Apache-2.0
const crypto = require("node:crypto");

// True if a Slath webhook request is genuine and recent.
function verify(secret, timestamp, signature, body) {
  if (Math.abs(Date.now() / 1000 - Number(timestamp)) > 300) return false;
  const mac = crypto.createHmac("sha256", secret);
  mac.update(`${timestamp}.`);
  mac.update(body);
  const expected = Buffer.from("sha256=" + mac.digest("hex"));
  const given = Buffer.from(signature);
  return expected.length === given.length && crypto.timingSafeEqual(expected, given);
}
```

Pass the body exactly as received (bytes, before any JSON parsing), and the
three header values as strings.
