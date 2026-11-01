// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Disclosure menus (<details class="menu">) work without JavaScript. This
// adds what users expect from menus: Escape closes the open menu and returns
// focus to its button, and a click or tap outside closes it.

function openMenus(): HTMLDetailsElement[] {
  return [...document.querySelectorAll<HTMLDetailsElement>("details.menu[open]")];
}

export function installMenus(): void {
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
