// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Suggestions for username fields: an ARIA combobox on the input with a
// listbox below it. As the person types, the last name of a
// comma-separated list (or, with `data-suggest-single`, the field's one
// name) is completed from /-/ui/references: people who can be mentioned in
// the repository, and with `data-suggest-teams` also teams of its
// organization (`org/team`). Up/Down choose, Enter or Tab accept, Escape
// closes. Without JavaScript the field is a plain text box.

interface Suggestion {
  value: string;
  label: string;
  detail: string | null;
  /** A person's uploaded avatar, if any. */
  avatar?: string | null;
}

let seq = 0;

function attach(input: HTMLInputElement): void {
  const repo = input.dataset.suggestUsers ?? "";
  const item = input.dataset.suggestItem ?? "";
  const single = input.dataset.suggestSingle !== undefined;
  const teams = input.dataset.suggestTeams !== undefined;
  const id = `user-suggestions-${++seq}`;
  const list = document.createElement("ul");
  list.id = id;
  list.className = "ac-list suggest-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", teams ? "People and teams" : "People");
  list.hidden = true;
  input.after(list);
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-autocomplete", "list");
  input.setAttribute("aria-expanded", "false");
  input.setAttribute("aria-controls", id);

  let items: Suggestion[] = [];
  let active = 0;
  let timer: number | undefined;
  let request = 0;

  const close = () => {
    list.hidden = true;
    items = [];
    input.setAttribute("aria-expanded", "false");
    input.removeAttribute("aria-activedescendant");
  };

  /** The names before the last one, and the last (partial) one. */
  const split = () => {
    if (single) return { done: [] as string[], last: input.value.trim() };
    const parts = input.value.split(",");
    const last = parts.pop() ?? "";
    return { done: parts.map((p) => p.trim()).filter(Boolean), last: last.trim() };
  };

  const accept = (i: number) => {
    const it = items[i];
    if (!it) return;
    const { done } = split();
    const name = it.value.replace(/^@/, "");
    input.value = single ? name : [...done.filter((d) => d !== name), name].join(", ") + ", ";
    close();
    input.focus();
    // A field for several names turns it into a chip (names.ts).
    input.dispatchEvent(new CustomEvent("names:accept", { detail: { name, avatar: it.avatar ?? null } }));
  };

  const render = () => {
    list.replaceChildren(
      ...items.map((it, i) => {
        const li = document.createElement("li");
        li.id = `${id}-${i}`;
        li.className = "ac-option";
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(i === active));
        const main = document.createElement("span");
        main.textContent = it.value.replace(/^@/, "");
        li.append(main);
        if (it.detail) {
          const d = document.createElement("span");
          d.className = "ac-detail";
          d.textContent = it.detail;
          li.append(d);
        }
        li.addEventListener("mousedown", (e) => e.preventDefault());
        li.addEventListener("click", () => accept(i));
        return li;
      }),
    );
    input.setAttribute("aria-expanded", "true");
    input.setAttribute("aria-activedescendant", `${id}-${active}`);
    list.style.top = `${input.offsetTop + input.offsetHeight + 4}px`;
    list.style.left = `${input.offsetLeft}px`;
  };

  input.addEventListener("input", () => {
    window.clearTimeout(timer);
    const { done, last } = split();
    const mine = ++request;
    timer = window.setTimeout(async () => {
      try {
        const ask = async (kind: string, q: string): Promise<Suggestion[]> => {
          const params = new URLSearchParams({ repo, kind, q });
          if (item) params.set("item", item);
          const r = await fetch(`/-/ui/references?${params}`, {
            credentials: "same-origin",
            headers: { accept: "application/json" },
          });
          return r.ok ? ((await r.json()) as { items: Suggestion[] }).items : [];
        };
        // Teams match by their name, after the organization's.
        const [people, teamHits] = await Promise.all([
          last.includes("/") ? Promise.resolve([]) : ask("user", last),
          teams ? ask("team", last.slice(last.indexOf("/") + 1)) : Promise.resolve([]),
        ]);
        const found = [...people, ...teamHits.map((t) => ({ ...t, detail: t.detail ?? "Team" }))];
        if (mine !== request) return;
        items = found.filter((f) => !done.includes(f.value.replace(/^@/, ""))).slice(0, 10);
      } catch {
        items = [];
      }
      if (items.length === 0) return close();
      active = 0;
      render();
      list.hidden = false;
    }, 150);
  });
  input.addEventListener("keydown", (e) => {
    if (list.hidden || items.length === 0) return;
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
  input.addEventListener("blur", () => window.setTimeout(close, 150));
}

export function installSuggestions(): void {
  const run = () =>
    document
      .querySelectorAll<HTMLInputElement>("input[data-suggest-users]:not([role=combobox])")
      .forEach(attach);
  run();
  document.addEventListener("htmx:after:swap", run);
}
