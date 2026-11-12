// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Hunk navigation by keyboard: while focus is inside a diff (#diff), n
// moves to the next change and p to the previous one. Single-key
// shortcuts work only there (WCAG 2.1.4); the "Previous change" and
// "Next change" links in each hunk's header do the same without them.

function editing(t: EventTarget | null): boolean {
  return (
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable)
  );
}

function hunks(diff: Element): HTMLElement[] {
  return [...diff.querySelectorAll<HTMLElement>(".diff-hunk")].filter((h) => h.offsetParent !== null);
}

/** The hunk to move to: the first below the current one, or the last above it. */
function target(list: HTMLElement[], current: HTMLElement | null, next: boolean): HTMLElement | undefined {
  if (current) {
    const i = list.indexOf(current);
    if (i >= 0) return list[next ? i + 1 : i - 1];
  }
  const edge = 8;
  return next
    ? list.find((h) => h.getBoundingClientRect().top > edge)
    : [...list].reverse().find((h) => h.getBoundingClientRect().top < -edge);
}

export function installHunkNavigation(): void {
  document.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey || editing(e.target)) return;
    if (e.key !== "n" && e.key !== "p") return;
    const diff = e.target instanceof Element ? e.target.closest("#diff") : null;
    if (!diff) return;
    const current = e.target instanceof Element ? e.target.closest<HTMLElement>(".diff-hunk") : null;
    const to = target(hunks(diff), current, e.key === "n");
    if (!to) return;
    e.preventDefault();
    to.scrollIntoView({ block: "start" });
    // Focus its scrollable region, so the next key continues from here.
    const region = [...to.querySelectorAll<HTMLElement>(".code-wrap")].find((r) => r.offsetParent !== null);
    region?.focus({ preventScroll: true });
  });
}
