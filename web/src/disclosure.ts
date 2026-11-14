// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Close buttons inside a disclosure (`button[data-close-details]`, such as
// the reply box's Cancel) fold it back and return focus to what opened
// it. Without scripts they stay hidden; the disclosure stays open.

function reveal(root: ParentNode): void {
  root.querySelectorAll<HTMLButtonElement>("button[data-close-details][hidden]").forEach((b) => {
    b.hidden = false;
  });
}

export function installCloseButtons(): void {
  reveal(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) reveal(e.target);
  });
  document.addEventListener("click", (e) => {
    const btn = e.target instanceof Element ? e.target.closest<HTMLButtonElement>("button[data-close-details]") : null;
    const details = btn?.closest("details");
    if (!btn || !details) return;
    details.open = false;
    details.querySelector<HTMLElement>(":scope > summary")?.focus();
  });
}
