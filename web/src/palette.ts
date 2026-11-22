// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The command palette (dialog#palette, in every page): Ctrl+K or ⌘K, or
// the global bar's button, opens it. It lists the page's own actions,
// collected from the page when it opens: the links and buttons of the
// global bar, the side menu, the tabs, the page heading's actions, and the
// footer, and every control with a shortcut (aria-keyshortcuts), each with
// its keys. Typing filters them; Up and Down choose, Enter or a click or tap
// takes one, Escape closes. Taking one follows that link or presses that
// button, so the palette never does more than the page. Ctrl+K inside the
// editor stays its link shortcut.

import { openDialog } from "./dialog";

interface Action {
  el: HTMLElement;
  label: string;
  where: string;
  keys: string;
}

/** Where the page's actions are, and what each place is called. */
const SOURCES: [string, string | null][] = [
  ["#site-header", "Global bar"],
  ["nav.app-sidebar", null],
  ["nav.tabs", null],
  [".page-head-actions", "This page"],
  [".site-footer", "Footer"],
];

/** An element's name as people read it. */
function nameOf(el: HTMLElement): string {
  const label = el.getAttribute("aria-label");
  if (label) return label.trim();
  return (el.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** How aria-keyshortcuts reads to people: Control+K as Ctrl+K. */
function keysOf(el: HTMLElement): string {
  return (el.getAttribute("aria-keyshortcuts") ?? "").split(" ")[0].replace(/Control/g, "Ctrl");
}

function usable(el: HTMLElement): boolean {
  if (el.closest("dialog, template, [hidden], [inert], [aria-hidden='true'], .menu-panel")) return false;
  if (el.matches(":disabled, [aria-disabled='true'], [aria-controls='palette']")) return false;
  // Links within the page are only actions when they're shortcuts (not
  // "Skip to main content" or the side menu's own toggles).
  if (el instanceof HTMLAnchorElement) {
    const href = el.getAttribute("href") ?? "";
    if (href.startsWith("#") && !el.hasAttribute("aria-keyshortcuts")) return false;
  }
  return nameOf(el) !== "";
}

/** The page's actions, each once (the narrow and wide bar repeat some). */
function collect(): Action[] {
  const out: Action[] = [];
  const seen = new Set<string>();
  const add = (el: HTMLElement, where: string) => {
    if (!usable(el)) return;
    const label = nameOf(el);
    const keys = keysOf(el);
    // A shortcut repeated in each of its components (every hunk's "Next
    // change") is one action; others are one per place they go.
    const target = keys || (el instanceof HTMLAnchorElement ? el.href : ((el as HTMLButtonElement).form?.action ?? ""));
    const key = `${label}\n${target}`;
    if (seen.has(key)) return;
    seen.add(key);
    out.push({ el, label, where, keys });
  };
  for (const [sel, name] of SOURCES) {
    for (const region of document.querySelectorAll<HTMLElement>(sel)) {
      const where = name ?? region.getAttribute("aria-label") ?? "This page";
      for (const el of region.querySelectorAll<HTMLElement>("a[href], button")) add(el, where);
    }
  }
  for (const el of document.querySelectorAll<HTMLElement>("#app-frame [aria-keyshortcuts]")) {
    if (el.matches("a[href], button")) add(el, "This page");
  }
  return out;
}

function matches(a: Action, query: string): boolean {
  const text = `${a.label} ${a.where}`.toLowerCase();
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((w) => text.includes(w));
}

let actions: Action[] = [];
let shown: Action[] = [];
let active = 0;

function parts(): { dialog: HTMLDialogElement; input: HTMLInputElement; list: HTMLElement; status: HTMLElement } | null {
  const dialog = document.getElementById("palette");
  const input = document.getElementById("palette-input");
  const list = document.getElementById("palette-list");
  const status = document.getElementById("palette-status");
  if (!(dialog instanceof HTMLDialogElement) || !(input instanceof HTMLInputElement) || !list || !status) return null;
  return { dialog, input, list, status };
}

function render(): void {
  const p = parts();
  if (!p) return;
  shown = actions.filter((a) => matches(a, p.input.value));
  active = Math.min(active, Math.max(shown.length - 1, 0));
  p.list.replaceChildren(
    ...shown.map((a, i) => {
      const li = document.createElement("li");
      li.id = `palette-option-${i}`;
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(i === active));
      const label = document.createElement("span");
      label.className = "palette-label";
      label.textContent = a.label;
      const where = document.createElement("span");
      where.className = "palette-where";
      where.textContent = a.where;
      li.append(label, where);
      if (a.keys) {
        const kbd = document.createElement("kbd");
        kbd.textContent = a.keys;
        li.append(kbd);
      }
      li.addEventListener("click", () => take(i));
      return li;
    }),
  );
  if (shown.length > 0) p.input.setAttribute("aria-activedescendant", `palette-option-${active}`);
  else p.input.removeAttribute("aria-activedescendant");
  p.status.textContent = shown.length === 1 ? "1 action" : `${shown.length} actions`;
  document.getElementById(`palette-option-${active}`)?.scrollIntoView({ block: "nearest" });
}

function take(i: number): void {
  const a = shown[i];
  const p = parts();
  if (!a || !p) return;
  // Closing first returns focus to the opener; the action then moves on.
  p.dialog.close();
  a.el.focus({ preventScroll: true });
  a.el.click();
}

function show(from: Element | null): void {
  const p = parts();
  if (!p || p.dialog.open) return;
  actions = collect();
  active = 0;
  p.input.value = "";
  render();
  openDialog("palette", from);
  p.input.focus();
}

export function installPalette(): void {
  document.addEventListener("keydown", (e) => {
    if (e.defaultPrevented || e.altKey || e.shiftKey || !(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "k") return;
    if (document.querySelector("dialog[open]")) return;
    e.preventDefault();
    show(document.activeElement);
  });
  // The bar's button: collect before the dialog script would open it.
  document.addEventListener(
    "click",
    (e) => {
      const b = e.target instanceof Element ? e.target.closest("button[aria-controls='palette']") : null;
      if (b) show(b);
    },
    true,
  );
  document.addEventListener("input", (e) => {
    if (e.target instanceof HTMLElement && e.target.id === "palette-input") {
      active = 0;
      render();
    }
  });
  document.addEventListener("keydown", (e) => {
    if (!(e.target instanceof HTMLElement) || e.target.id !== "palette-input") return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (shown.length === 0) return;
      active = (active + (e.key === "ArrowDown" ? 1 : shown.length - 1)) % shown.length;
      render();
    } else if (e.key === "Enter") {
      e.preventDefault();
      take(active);
    }
  });
}
