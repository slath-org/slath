// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Suggestions for a brief item's Points at field (`textarea[data-places]`),
// one place per line: as the person types, the line at the caret is
// completed from the places the form lists for its diff
// (`[data-brief-places]`, one per line), the ones starting with what's
// typed first. Down opens the list on an empty line too; Up/Down choose,
// Enter or Tab accept, Escape closes; a tap or click accepts. Without
// JavaScript the list on the form is there to copy from.

let seq = 0;

/** The line of `field` the caret is on: its start, end, and text. */
function lineAt(field: HTMLTextAreaElement): { start: number; end: number; text: string } {
  const v = field.value;
  const caret = field.selectionStart ?? v.length;
  const start = v.lastIndexOf("\n", caret - 1) + 1;
  const next = v.indexOf("\n", caret);
  const end = next === -1 ? v.length : next;
  return { start, end, text: v.slice(start, end) };
}

/** The places matching `typed`: those starting with it, then those that
 * contain it, without the one typed in full. */
export function matches(places: string[], typed: string): string[] {
  const q = typed.trim().toLowerCase();
  const starts = places.filter((p) => p.toLowerCase().startsWith(q));
  const contains = places.filter((p) => !p.toLowerCase().startsWith(q) && p.toLowerCase().includes(q));
  return [...starts, ...contains].filter((p) => p.toLowerCase() !== q).slice(0, 10);
}

function attach(field: HTMLTextAreaElement, places: string[]): void {
  const id = `place-suggestions-${++seq}`;
  const list = document.createElement("ul");
  list.id = id;
  list.className = "ac-list suggest-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", "Places in this diff");
  list.hidden = true;
  field.after(list);
  field.setAttribute("aria-autocomplete", "list");
  field.setAttribute("aria-controls", id);

  let items: string[] = [];
  let active = 0;

  const close = () => {
    list.hidden = true;
    items = [];
    field.removeAttribute("aria-activedescendant");
  };

  const accept = (i: number) => {
    const value = items[i];
    if (value === undefined) return;
    const { start, end } = lineAt(field);
    field.value = field.value.slice(0, start) + value + field.value.slice(end);
    const caret = start + value.length;
    field.setSelectionRange(caret, caret);
    close();
    field.focus();
  };

  const render = () => {
    list.replaceChildren(
      ...items.map((value, i) => {
        const li = document.createElement("li");
        li.id = `${id}-${i}`;
        li.className = "ac-option";
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(i === active));
        li.textContent = value;
        li.addEventListener("mousedown", (e) => e.preventDefault());
        li.addEventListener("click", () => accept(i));
        return li;
      }),
    );
    field.setAttribute("aria-activedescendant", `${id}-${active}`);
    list.style.top = `${field.offsetTop + field.offsetHeight + 4}px`;
    list.style.left = `${field.offsetLeft}px`;
    list.hidden = false;
  };

  const show = (all: boolean) => {
    const { text } = lineAt(field);
    if (!all && text.trim() === "") return close();
    items = matches(places, text);
    if (items.length === 0) return close();
    active = 0;
    render();
  };

  field.addEventListener("input", () => show(false));
  field.addEventListener("keydown", (e) => {
    if (list.hidden || items.length === 0) {
      if (e.key === "ArrowDown" && lineAt(field).text.trim() === "") {
        e.preventDefault();
        show(true);
      }
      return;
    }
    let handled = true;
    if (e.key === "ArrowDown") active = (active + 1) % items.length;
    else if (e.key === "ArrowUp") active = (active - 1 + items.length) % items.length;
    else if (e.key === "Enter" || e.key === "Tab") accept(active);
    else if (e.key === "Escape") close();
    else handled = false;
    if (handled) {
      e.preventDefault();
      if (!list.hidden) render();
    }
  });
  field.addEventListener("blur", () => window.setTimeout(close, 150));
}

export function installPlaceSuggestions(): void {
  const run = () => {
    const source = document.querySelector<HTMLElement>("[data-brief-places]");
    if (!source) return;
    const places = (source.textContent ?? "")
      .split("\n")
      .map((l) => l.trim())
      .filter(Boolean);
    document
      .querySelectorAll<HTMLTextAreaElement>("textarea[data-places]:not([aria-controls])")
      .forEach((f) => attach(f, places));
  };
  run();
  // Pages that arrive by htmx (links, and the form after Add or an error).
  document.addEventListener("htmx:after:swap", run);
}
