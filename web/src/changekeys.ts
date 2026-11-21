// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Moving through a stack by keyboard on a change page: j and k go to the
// next and previous change, ] and [ to the next and previous version.
// They follow the page's own "Next change" (and similar) links, marked
// with data-key, so the keys never do more than the buttons. Single-key
// shortcuts act only while focus is inside the change view
// ([data-change-keys]) (WCAG 2.1.4). After a move by key, focus goes to
// the same button on the new page, so the next key continues from there.

const KEYS = new Set(["j", "k", "[", "]"]);
let pending: string | null = null;

function editing(t: EventTarget | null): boolean {
  return (
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable)
  );
}

function link(view: Element, key: string): HTMLAnchorElement | null {
  return view.querySelector<HTMLAnchorElement>(`a[data-key="${CSS.escape(key)}"]`);
}

/** Focus the button the last key used, or the view's first button. */
function refocus(): void {
  if (!pending) return;
  const key = pending;
  pending = null;
  const view = document.querySelector<HTMLElement>("[data-change-keys]");
  if (!view) return;
  const to = link(view, key) ?? view.querySelector<HTMLElement>("a[data-key]");
  to?.focus();
}

export function installChangeKeys(): void {
  document.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey || !KEYS.has(e.key) || editing(e.target)) return;
    const view = e.target instanceof Element ? e.target.closest("[data-change-keys]") : null;
    if (!view) return;
    const to = link(view, e.key);
    if (!to) return;
    e.preventDefault();
    pending = e.key;
    to.click();
  });
  document.addEventListener("htmx:after:swap", refocus);
  // Without the frame's navigation, the new page loads in full.
  window.addEventListener("pagehide", () => {
    try {
      if (pending) sessionStorage.setItem("slath-change-key", pending);
    } catch {
      // No storage: the next page starts without focus on the button.
    }
  });
  try {
    const key = sessionStorage.getItem("slath-change-key");
    sessionStorage.removeItem("slath-change-key");
    if (key && KEYS.has(key)) {
      pending = key;
      refocus();
    }
  } catch {
    // No storage: focus stays at the page's start.
  }
}
