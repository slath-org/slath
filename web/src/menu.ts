// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Disclosure menus (<details class="menu">) work without JavaScript. This
// adds what users expect from menus: Escape closes the open menu and returns
// focus to its button, a click or tap outside closes it, and an open panel
// is shifted to stay inside the viewport on narrow screens.

/** Space kept between a panel and the viewport edge (--space-4). */
const EDGE = 16;

function fitPanel(menu: HTMLDetailsElement): void {
  const panel = menu.querySelector<HTMLElement>(":scope > .menu-panel");
  if (!panel) return;
  panel.style.translate = "";
  if (!menu.open) return;
  const r = panel.getBoundingClientRect();
  const max = document.documentElement.clientWidth - EDGE;
  let dx = 0;
  if (r.left < EDGE) dx = EDGE - r.left;
  else if (r.right > max) dx = Math.max(max - r.right, EDGE - r.left);
  if (dx !== 0) panel.style.translate = `${dx}px 0`;
}

function openMenus(): HTMLDetailsElement[] {
  return [...document.querySelectorAll<HTMLDetailsElement>("details.menu[open]")];
}

export function installMenus(): void {
  // `toggle` doesn't bubble; a capturing listener sees every menu.
  document.addEventListener(
    "toggle",
    (evt) => {
      if (evt.target instanceof HTMLDetailsElement && evt.target.matches("details.menu")) {
        fitPanel(evt.target);
      }
    },
    true,
  );
  window.addEventListener("resize", () => openMenus().forEach(fitPanel));
  document.addEventListener("keydown", (evt) => {
    if (evt.key !== "Escape") return;
    for (const menu of openMenus()) {
      if (menu.contains(document.activeElement) || menu.matches(":focus-within")) {
        menu.open = false;
        menu.querySelector<HTMLElement>("summary")?.focus();
        evt.preventDefault();
      } else {
        menu.open = false;
      }
    }
  });
  document.addEventListener("pointerdown", (evt) => {
    for (const menu of openMenus()) {
      if (!(evt.target instanceof Node) || !menu.contains(evt.target)) menu.open = false;
    }
  });
}
