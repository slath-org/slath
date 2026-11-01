// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Accessibility around htmx swaps: aria-busy on the target while a request
// runs, and focus that never falls back to <body> when the focused element
// is replaced.

interface SwapDetail {
  ctx?: { target?: Element | null };
}

function target(evt: Event): HTMLElement | null {
  const t = (evt as CustomEvent<SwapDetail>).detail?.ctx?.target;
  return t instanceof HTMLElement ? t : null;
}

export function installA11ySwaps(): void {
  let hadFocus = false;
  document.addEventListener("htmx:before:request", (evt) => {
    const t = target(evt);
    if (!t) return;
    t.setAttribute("aria-busy", "true");
    hadFocus = t.contains(document.activeElement);
  });
  document.addEventListener("htmx:after:swap", (evt) => {
    const t = target(evt);
    if (!t) return;
    t.removeAttribute("aria-busy");
    if (!hadFocus || (document.activeElement && document.activeElement !== document.body)) return;
    // The focused element was replaced: move focus to the new error summary,
    // the region's heading, or the region itself.
    const next =
      t.querySelector<HTMLElement>("[data-error-summary]") ??
      t.querySelector<HTMLElement>("h1, h2, h3, [data-focus-target]") ??
      t;
    if (!next.hasAttribute("tabindex") && !next.matches("a[href], button, input, select, textarea")) {
      next.setAttribute("tabindex", "-1");
    }
    next.focus();
  });
  document.addEventListener("htmx:error", (evt) => target(evt)?.removeAttribute("aria-busy"));
}
