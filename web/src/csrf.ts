// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Adds the page's CSRF token to every htmx request as X-CSRF-Token.
// Forms also carry it as a hidden field, so they work without JavaScript.

interface ConfigRequestDetail {
  ctx?: { request?: { headers?: Record<string, string> } };
}

export function csrfToken(): string | null {
  const meta = document.querySelector<HTMLMetaElement>('meta[name="csrf-token"]');
  return meta?.content || null;
}

export function installCsrf(): void {
  document.addEventListener("htmx:config:request", (evt) => {
    const token = csrfToken();
    const headers = (evt as CustomEvent<ConfigRequestDetail>).detail?.ctx?.request?.headers;
    if (token && headers) {
      headers["X-CSRF-Token"] = token;
    }
  });
}
