// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The editor's one Trusted Types policy ("slath-editor"). It passes HTML
// through unchanged, so callers may only give it HTML the server sanitized
// (the preview) or input they validated themselves (single named entities).

interface Policy {
  createHTML(s: string): string;
}

let policy: Policy | null | undefined;

export function trustedHTML(html: string): string {
  if (policy === undefined) {
    const tt = (window as unknown as { trustedTypes?: { createPolicy(name: string, rules: object): Policy } })
      .trustedTypes;
    policy = tt ? tt.createPolicy("slath-editor", { createHTML: (s: string) => s }) : null;
  }
  return policy ? (policy.createHTML(html) as unknown as string) : html;
}
