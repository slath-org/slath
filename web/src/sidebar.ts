// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The side menu (#app-sidebar) and its toggle in the global bar. Without
// JavaScript the toggle is a link to the menu: wide screens always show it,
// narrow ones show it as a drawer while it's the URL's target. With this
// script the toggle collapses the menu on wide screens (remembered in a
// cookie, so the server renders the next page the same way) and opens and
// closes the drawer on narrow ones.

const WIDE = "(min-width: 64rem)";
const COOKIE = "slath_sidebar";

function sidebar(): HTMLElement | null {
  return document.getElementById("app-sidebar");
}

function toggles(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("[data-sidebar-toggle]")];
}

function wide(): boolean {
  return matchMedia(WIDE).matches;
}

function drawerOpen(menu: HTMLElement): boolean {
  return menu.hasAttribute("data-open") || location.hash === "#app-sidebar";
}

/** Whether the menu is visible now. */
function expanded(menu: HTMLElement): boolean {
  return wide() ? document.documentElement.dataset.sidebar !== "collapsed" : drawerOpen(menu);
}

function sync(menu: HTMLElement): void {
  const open = expanded(menu);
  for (const t of toggles()) t.setAttribute("aria-expanded", String(open));
}

function closeDrawer(menu: HTMLElement, focusToggle: boolean): void {
  menu.removeAttribute("data-open");
  // Drop a #app-sidebar target left from before the script ran.
  if (location.hash === "#app-sidebar") history.replaceState(null, "", location.pathname + location.search);
  sync(menu);
  if (focusToggle) toggles()[0]?.focus();
}

function toggle(menu: HTMLElement): void {
  if (wide()) {
    const collapse = document.documentElement.dataset.sidebar !== "collapsed";
    if (collapse) document.documentElement.dataset.sidebar = "collapsed";
    else delete document.documentElement.dataset.sidebar;
    document.cookie = collapse
      ? `${COOKIE}=collapsed; path=/; max-age=31536000; samesite=lax`
      : `${COOKIE}=; path=/; max-age=0; samesite=lax`;
    sync(menu);
  } else if (drawerOpen(menu)) {
    closeDrawer(menu, false);
  } else {
    menu.setAttribute("data-open", "");
    sync(menu);
    menu.querySelector<HTMLElement>('[aria-current="page"], .app-sidebar-list a')?.focus();
  }
}

export function installSidebar(): void {
  const update = () => {
    const menu = sidebar();
    for (const t of toggles()) {
      t.hidden = !menu;
      t.setAttribute("role", "button");
    }
    if (menu) sync(menu);
  };
  update();
  // Navigation inside the frame replaces the menu (or removes it).
  document.addEventListener("htmx:after:swap", update);
  matchMedia(WIDE).addEventListener("change", () => {
    sidebar()?.removeAttribute("data-open");
    update();
  });
  document.addEventListener("click", (evt) => {
    const menu = sidebar();
    const target = evt.target instanceof Element ? evt.target : null;
    if (!menu) return;
    if (target?.closest("[data-sidebar-toggle]")) {
      evt.preventDefault();
      toggle(menu);
    } else if (target?.closest("[data-sidebar-close]")) {
      evt.preventDefault();
      closeDrawer(menu, true);
    }
  });
  document.addEventListener("keydown", (evt) => {
    const menu = sidebar();
    if (!menu) return;
    if (evt.key === "Escape" && !wide() && drawerOpen(menu)) {
      closeDrawer(menu, menu.contains(document.activeElement));
    } else if (evt.key === " " && document.activeElement?.matches("[data-sidebar-toggle]")) {
      // A link acting as a button also answers to Space.
      evt.preventDefault();
      toggle(menu);
    }
  });
  document.addEventListener("pointerdown", (evt) => {
    const menu = sidebar();
    if (!menu || wide() || !drawerOpen(menu) || !(evt.target instanceof Element)) return;
    if (!menu.contains(evt.target) && !evt.target.closest("[data-sidebar-toggle]")) closeDrawer(menu, false);
  });
}
