// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Hunk navigation by keyboard: while focus is inside a diff (#diff), n
// moves to the next change and p to the previous one, v to the next file
// not marked viewed and t to the next unresolved thread. Single-key
// shortcuts work only there (WCAG 2.1.4); the "Previous change" and
// "Next change" links in each hunk's header, and "Next unviewed file" and
// "Next unresolved thread" above the files, do the same without them
// (those two go to the first one without this script).

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

/** The next file not marked viewed, or unresolved thread, below the view. */
function nextReview(diff: Element, kind: "unviewed" | "unresolved"): HTMLElement | undefined {
  const sel = kind === "unviewed" ? ".diff-file[data-unviewed]" : ".diff-thread-row[data-unresolved]";
  const list = [...diff.querySelectorAll<HTMLElement>(sel)].filter((x) => x.offsetParent !== null);
  return list.find((x) => x.getBoundingClientRect().top > 8) ?? list[0];
}

function goTo(to: HTMLElement): void {
  to.scrollIntoView({ block: "start" });
  if (!to.hasAttribute("tabindex")) to.setAttribute("tabindex", "-1");
  to.focus({ preventScroll: true });
}

export function installHunkNavigation(): void {
  document.addEventListener("click", (e) => {
    const link = e.target instanceof Element ? e.target.closest<HTMLAnchorElement>("a[data-review-next]") : null;
    const diff = document.getElementById("diff");
    if (!link || !diff) return;
    const to = nextReview(diff, link.dataset.reviewNext === "unresolved" ? "unresolved" : "unviewed");
    if (!to) return;
    e.preventDefault();
    goTo(to);
  });
  document.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey || editing(e.target)) return;
    if (e.key === "v" || e.key === "t") {
      const diff = e.target instanceof Element ? e.target.closest("#diff") : null;
      const to = diff ? nextReview(diff, e.key === "v" ? "unviewed" : "unresolved") : undefined;
      if (!to) return;
      e.preventDefault();
      goTo(to);
      return;
    }
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
