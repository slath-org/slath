// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// UI strings used by scripts. Page text lives in the server templates.

const strings = {
  retry: "Retry",
  dismiss: "Dismiss",
  requestFailed: "The request failed. Check your connection and try again.",
  requestFailedStatus: "The request failed (HTTP {0}). Try again.",
  passkeyFailed: "The passkey request failed. Reload the page and try again.",
  passkeyCancelled: "No passkey was used. Try again, or use another way.",
} as const;

export type Key = keyof typeof strings;

export function t(key: Key, ...args: string[]): string {
  return args.reduce<string>((s, a, i) => s.replace(`{${i}}`, a), strings[key]);
}
