// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Hidden lines of a diff: a link that shows them (`a[data-expand]`)
// replaces its file's section in place (htmx). Afterwards focus goes to
// the hunk the lines are next to, not to the top of the file. Without
// this script the link opens the file alone at that hunk.

export function installExpansion(): void {
  let hunk: string | null = null;
  document.addEventListener("click", (e) => {
    const a = e.target instanceof Element ? e.target.closest<HTMLElement>("a[data-expand]") : null;
    if (a) hunk = a.dataset.expand ?? null;
  });
  document.addEventListener("htmx:after:swap", () => {
    if (!hunk) return;
    const id = hunk;
    hunk = null;
    const region = [...(document.getElementById(id)?.querySelectorAll<HTMLElement>(".code-wrap") ?? [])].find(
      (r) => r.offsetParent !== null,
    );
    region?.focus();
  });
}
