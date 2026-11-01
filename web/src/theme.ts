// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Theme menu enhancement. The server renders data-theme on <html> from the
// user setting or the slath_theme cookie, so the first paint is right; this
// module only applies a new choice immediately, before the form round trip.

export function installTheme(): void {
  document.addEventListener("change", (evt) => {
    const input = evt.target;
    if (!(input instanceof HTMLInputElement) || input.name !== "theme" || !input.form?.matches("[data-theme-form]")) {
      return;
    }
    const value = input.value;
    if (value === "light" || value === "dark") {
      document.documentElement.dataset.theme = value;
    } else {
      delete document.documentElement.dataset.theme;
    }
  });
}
