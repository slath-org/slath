// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Part of the main bundle: finds <slath-editor> elements and loads the
// editor only on pages that have one. Signing out removes drafts.
import { clearAllDrafts } from "./drafts";

function enhance(): void {
  const hosts = Array.from(document.querySelectorAll<HTMLElement>("slath-editor:not([data-ready])"));
  if (hosts.length === 0) return;
  import("./shell")
    .then((m) => Promise.all(hosts.map((h) => m.mount(h))))
    .catch(() => {
      // The plain textarea keeps working.
    });
}

export function installEditors(): void {
  enhance();
  document.addEventListener("htmx:after:swap", enhance);
  document.addEventListener(
    "submit",
    (e) => {
      const form = e.target;
      if (form instanceof HTMLFormElement && new URL(form.action, window.location.href).pathname === "/-/logout") {
        clearAllDrafts();
      }
    },
    true,
  );
}
