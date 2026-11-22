// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Weave dialogs: a button[data-dialog] opens the dialog its aria-controls
// names, as a modal; when it closes, focus goes back to what had it.
// Such buttons may start hidden (dialogs need this script), and show once
// it runs.

/** Opens dialog `id`, returning focus to `from` when it closes. */
export function openDialog(id: string, from: Element | null): void {
  const dialog = document.getElementById(id);
  if (!(dialog instanceof HTMLDialogElement) || dialog.open) return;
  dialog.showModal();
  const back = from instanceof HTMLElement ? from : null;
  dialog.addEventListener("close", () => back?.focus(), { once: true });
}

function reveal(): void {
  for (const b of document.querySelectorAll<HTMLButtonElement>("button[data-dialog][hidden]")) b.hidden = false;
}

export function installDialogs(): void {
  reveal();
  document.addEventListener("htmx:after:swap", reveal);
  document.addEventListener("click", (e) => {
    const b = e.target instanceof Element ? e.target.closest("button[data-dialog]") : null;
    const id = b?.getAttribute("aria-controls");
    if (b && id) openDialog(id, b);
  });
}
