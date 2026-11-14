// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Fields for several names (`input[data-names]`: reviewers, assignees,
// labels). Without scripts they're plain text, names separated by commas.
// This shows each chosen name as a removable chip with its avatar, and
// keeps the comma-separated value in a hidden field for the form. A name
// is added by choosing a suggestion, typing a comma, Enter, or leaving
// the field; Backspace in the empty field removes the last chip.

import { avatar } from "./avatar";

function attach(input: HTMLInputElement): void {
  const people = input.dataset.names !== "labels";
  const hidden = document.createElement("input");
  hidden.type = "hidden";
  hidden.name = input.name;
  input.removeAttribute("name");
  const field = document.createElement("div");
  field.className = "names-field";
  const chips = document.createElement("ul");
  chips.className = "names-chips";
  chips.setAttribute("aria-label", "Chosen");
  input.before(field);
  field.append(chips, input, hidden);
  const names: string[] = [];

  const sync = () => {
    hidden.value = names.join(", ");
    chips.hidden = names.length === 0;
  };

  const add = async (raw: string, url: string | null = null) => {
    const name = raw.trim().replace(/^@/, "");
    if (!name || names.some((n) => n.toLowerCase() === name.toLowerCase())) return;
    names.push(name);
    const li = document.createElement("li");
    li.className = "names-chip";
    if (people) li.append(await avatar(name, url));
    const label = document.createElement("bdi");
    label.textContent = name;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "names-remove";
    remove.setAttribute("aria-label", `Remove ${name}`);
    remove.textContent = "×";
    remove.addEventListener("click", () => {
      names.splice(names.indexOf(name), 1);
      li.remove();
      sync();
      input.focus();
    });
    li.append(label, remove);
    chips.append(li);
    sync();
  };

  /** Turns everything before the last comma into chips. */
  const take = (all = false) => {
    const parts = input.value.split(",");
    const rest = all ? "" : (parts.pop() ?? "");
    for (const p of all ? parts : parts) void add(p);
    input.value = rest.trimStart();
  };

  for (const n of input.value.split(",")) void add(n);
  input.value = "";
  sync();

  input.addEventListener("names:accept", (e) => {
    const { name, avatar: url } = (e as CustomEvent<{ name: string; avatar: string | null }>).detail;
    input.value = "";
    void add(name, url);
  });
  input.addEventListener("input", () => {
    if (input.value.includes(",")) take();
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && input.value.trim() !== "" && input.getAttribute("aria-expanded") !== "true") {
      e.preventDefault();
      void add(input.value);
      input.value = "";
    } else if (e.key === "Backspace" && input.value === "" && names.length > 0) {
      names.pop();
      chips.lastElementChild?.remove();
      sync();
    }
  });
  input.addEventListener("blur", () => {
    window.setTimeout(() => {
      if (document.activeElement !== input && input.value.trim() !== "") {
        void add(input.value);
        input.value = "";
      }
    }, 200);
  });
  // A form sent while a name is still being typed keeps it.
  input.form?.addEventListener("submit", () => {
    if (input.value.trim() !== "") {
      void add(input.value);
      input.value = "";
    }
    hidden.value = names.join(", ");
  });
}

export function installNamesFields(): void {
  const run = (root: ParentNode) =>
    root.querySelectorAll<HTMLInputElement>("input[data-names]:not([data-names-ready])").forEach((i) => {
      i.dataset.namesReady = "";
      attach(i);
    });
  run(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) run(e.target);
  });
}
