// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Appearance menu enhancement. The server renders data-theme and
// data-density on <html> from the user setting or cookie, so the first paint
// is right. Without JavaScript the menu's "Apply" button submits the form;
// with it, a choice applies at once and is saved in the background.

/** Shows the favicon of the chosen theme, or the system scheme's. */
function favicon(theme: string): void {
  for (const link of document.querySelectorAll<HTMLLinkElement>('link[rel="icon"][data-scheme]')) {
    const scheme = link.dataset.scheme ?? "";
    const media =
      theme === "light" || theme === "dark"
        ? theme === scheme
          ? "all"
          : "not all"
        : `(prefers-color-scheme: ${scheme})`;
    if (link.media === media) continue;
    // Browsers pick up a changed icon reliably only from a new element.
    const fresh = link.cloneNode() as HTMLLinkElement;
    fresh.media = media;
    link.replaceWith(fresh);
  }
}

function apply(name: string, value: string): void {
  const root = document.documentElement;
  if (name === "theme") {
    if (value === "light" || value === "dark") root.dataset.theme = value;
    else delete root.dataset.theme;
    favicon(value);
  } else if (name === "density" && (value === "comfortable" || value === "compact")) {
    root.dataset.density = value;
  }
}

export function installTheme(): void {
  document.addEventListener("change", (evt) => {
    const input = evt.target;
    const form = input instanceof HTMLInputElement ? input.form : null;
    if (!(input instanceof HTMLInputElement) || !form?.matches("[data-theme-form]")) return;
    apply(input.name, input.value);
    const body = new URLSearchParams();
    for (const [k, v] of new FormData(form)) if (typeof v === "string") body.append(k, v);
    void fetch(form.action, { method: "POST", body, redirect: "manual", credentials: "same-origin" });
  });
}
