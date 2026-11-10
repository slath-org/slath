// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Choices between alternatives (`.choice` with radios, CSS shows the chosen
// panel): radios with `data-remember="KEY"` keep the last choice in this
// browser, e.g. Git or jj, and choosing in one group sets the others with
// the same key on the page. Without this script, the default is chosen.

const PREFIX = "slath.choice.";

function load(key: string): string | null {
  try {
    return localStorage.getItem(PREFIX + key);
  } catch {
    return null;
  }
}

function save(key: string, value: string): void {
  try {
    localStorage.setItem(PREFIX + key, value);
  } catch {
    // Private windows and blocked storage: the choice lasts for the page.
  }
}

function apply(root: ParentNode): void {
  root
    .querySelectorAll<HTMLInputElement>("input[type=radio][data-remember]")
    .forEach((r) => {
      if (load(r.dataset.remember ?? "") === r.value) r.checked = true;
    });
}

export function installChoices(): void {
  apply(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) apply(e.target);
  });
  document.addEventListener("change", (e) => {
    const r = e.target;
    if (!(r instanceof HTMLInputElement) || r.type !== "radio") return;
    const key = r.dataset.remember;
    if (!key) return;
    save(key, r.value);
    document
      .querySelectorAll<HTMLInputElement>(`input[type=radio][data-remember="${CSS.escape(key)}"]`)
      .forEach((o) => {
        if (o.value === r.value) o.checked = true;
      });
  });
}
