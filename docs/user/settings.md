<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Appearance and time zone

**Settings → Appearance** holds how Slath looks for you. The choices belong
to your account, so they follow you to every device you sign in on.

- **Theme**: System (follows your device), Light, or Dark.
- **Density**: Comfortable or Compact. Compact has less padding; text and
  touch targets keep their size.
- **Code font size**: 12 to 20 pixels, for code, diffs, and commands.
- **Time zone**: the zone pages and emails show times in.
- **Commands for**: whether pages show checkout and conflict commands for
  Git or for jj. Automatic follows what your recent pushes came from. See
  [Merge requests](merge-requests.md).
- **Keyboard shortcuts**: All, or Only with Ctrl or ⌘. See
  [Keyboard shortcuts](#keyboard-shortcuts).

## Time zones

Until you choose one, times are in UTC. When you open the page, it
proposes the zone of the device you're using ("Proposed from this device:
Europe/Berlin"); nothing changes until you save. The list is the IANA time
zone database built into Slath, by region; `Etc` holds fixed offsets such
as UTC−12.

With a zone chosen:

- Pages show times in it, correct across daylight saving changes:
  recent ones as "3 hours ago", older ones as a date ("14 Nov 2026"),
  with the exact time and the zone's abbreviation or offset in a tooltip
  ("2026-11-14 10:05 CET", "2026-11-14 14:50 UTC+05:45").
- Emails Slath sends you, such as security notices, use it too.
- Dates without a time, such as a milestone's due date, are calendar
  dates: the same day for everyone, whatever their zone.
- The API always returns times in UTC (RFC 3339), whatever your setting.
- Scheduled work, such as an instance's daily backup, runs at a time
  stated in UTC.

## Keyboard shortcuts

Some shortcuts are a single key, such as **j** for the next change of a
stack or **n** for the next change in a diff. They act only where they're
listed, never while you type in a field, and every one of them has a
button that does the same.

If you use speech input, or keys get pressed by mistake, choose **Only
with Ctrl or ⌘** under **Keyboard shortcuts**: single keys then do
nothing, and shortcuts with Ctrl or ⌘, such as Ctrl+Enter to send a
comment, keep working. Without an account, the choice is kept in a cookie
of your browser.
