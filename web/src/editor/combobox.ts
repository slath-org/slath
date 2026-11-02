// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Reference autocomplete as an ARIA combobox: the editing element keeps
// role=textbox and gets aria-autocomplete, aria-controls, and
// aria-activedescendant; suggestions are a role=listbox in the same tree
// (so the IDREFs resolve, also inside a shadow root). Keys are taken on the
// editor wrapper before the editing mode sees them: Up/Down move, Enter or
// Tab accept, Escape closes. Suggestions come from /-/ui/references,
// debounced, at most 10.
import type { Mode } from "./mode";

interface Suggestion {
  kind: string;
  value: string;
  label: string;
  detail: string | null;
}

/** A trigger before the caret: `@na`, `#12`, `~bu`, `%v1`, `ns/repo#4`. */
const TRIGGER = /(?:^|[\s(])((?:([\w.-]+\/[\w.-]+)#)|[@#~%])([\w./-]*)$/;

const KINDS: Record<string, string> = { "@": "user", "#": "item", "~": "label", "%": "milestone" };

let seq = 0;

export interface Combobox {
  update(): void;
  close(): void;
  destroy(): void;
}

export function attachCombobox(mode: Mode, wrapper: HTMLElement, repo: string, item: string, status: HTMLElement): Combobox {
  const id = `editor-suggestions-${++seq}`;
  const list = document.createElement("ul");
  list.id = id;
  list.className = "ac-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", "Suggestions");
  list.hidden = true;
  mode.popupParent.appendChild(list);
  const root = mode.root;
  root.setAttribute("aria-autocomplete", "list");
  root.setAttribute("aria-haspopup", "listbox");

  let items: Suggestion[] = [];
  let active = 0;
  let matchLen = 0;
  let timer: number | undefined;
  let request = 0;

  const close = () => {
    list.hidden = true;
    items = [];
    root.removeAttribute("aria-activedescendant");
    root.removeAttribute("aria-controls");
  };

  const accept = (i: number) => {
    const it = items[i];
    if (!it) return;
    mode.replaceBeforeCaret(matchLen, `${it.value} `);
    status.textContent = `Inserted ${it.value}`;
    close();
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
        main.textContent = it.value;
        li.append(main);
        if (it.detail || it.label !== it.value.replace(/^[@#~%]"?|"$/g, "")) {
          const det = document.createElement("span");
          det.className = "ac-detail";
          det.textContent = it.detail ?? it.label;
          li.append(det);
        }
        li.addEventListener("pointerdown", (e) => {
          if (e.pointerType !== "touch") e.preventDefault();
        });
        li.addEventListener("mousedown", (e) => e.preventDefault());
        li.addEventListener("click", () => accept(i));
        return li;
      }),
    );
    root.setAttribute("aria-controls", id);
    root.setAttribute("aria-activedescendant", `${id}-${active}`);
  };

  const position = () => {
    const c = mode.caretRect();
    if (!c) return;
    const a = mode.anchor.getBoundingClientRect();
    list.style.left = `${Math.max(0, c.left - a.left)}px`;
    list.style.top = `${c.bottom - a.top + 4}px`;
  };

  const fetchSuggestions = async (repoPath: string, kind: string, q: string): Promise<Suggestion[]> => {
    const params = new URLSearchParams({ repo: repoPath, kind, q });
    if (item) params.set("item", item);
    const r = await fetch(`/-/ui/references?${params}`, {
      credentials: "same-origin",
      headers: { accept: "application/json" },
    });
    if (!r.ok) return [];
    const j = (await r.json()) as { items: Suggestion[] };
    return j.items ?? [];
  };

  const update = () => {
    window.clearTimeout(timer);
    const before = mode.textBeforeCaret();
    const m = before === null ? null : TRIGGER.exec(before);
    if (!m || !repo) return close();
    const crossRepo = m[2];
    const sigil = crossRepo ? "#" : m[1];
    const q = m[3];
    let kind = KINDS[sigil] ?? "user";
    if (sigil === "@" && q.includes("/")) kind = "team";
    const target = crossRepo ?? repo;
    const len = m[1].length + q.length;
    const mine = ++request;
    timer = window.setTimeout(async () => {
      let found: Suggestion[] = [];
      try {
        found = await fetchSuggestions(target, kind, q);
      } catch {
        found = [];
      }
      if (mine !== request) return;
      if (crossRepo) found = found.map((f) => ({ ...f, value: `${crossRepo}${f.value}` }));
      if (found.length === 0) return close();
      const wasOpen = !list.hidden;
      items = found.slice(0, 10);
      matchLen = len;
      if (!wasOpen) active = 0;
      active = Math.min(active, items.length - 1);
      render();
      list.hidden = false;
      position();
      status.textContent = `${items.length} suggestion${items.length === 1 ? "" : "s"}`;
    }, 150);
  };

  const onKey = (e: KeyboardEvent) => {
    if (list.hidden || items.length === 0) return;
    let handled = true;
    if (e.key === "ArrowDown") active = (active + 1) % items.length;
    else if (e.key === "ArrowUp") active = (active - 1 + items.length) % items.length;
    else if (e.key === "Enter" || e.key === "Tab") accept(active);
    else if (e.key === "Escape") close();
    else handled = false;
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
      if (!list.hidden) render();
    }
  };
  const onBlur = () => window.setTimeout(close, 150);
  wrapper.addEventListener("keydown", onKey, true);
  root.addEventListener("blur", onBlur);
  return {
    update,
    close,
    destroy() {
      wrapper.removeEventListener("keydown", onKey, true);
      root.removeEventListener("blur", onBlur);
      list.remove();
    },
  };
}
