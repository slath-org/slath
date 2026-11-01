// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Native <dialog> wiring: a button with data-dialog-open="<id>" opens the
// dialog as modal (focus moves into it, Escape closes it), and buttons with
// data-dialog-close close it. Focus returns to the opener.

export function installDialogs(): void {
  document.addEventListener("click", (evt) => {
    const target = evt.target instanceof Element ? evt.target : null;
    const opener = target?.closest<HTMLElement>("[data-dialog-open]");
    if (opener) {
      const dialog = document.getElementById(opener.dataset.dialogOpen ?? "");
      if (dialog instanceof HTMLDialogElement && !dialog.open) {
        dialog.showModal();
        dialog.addEventListener("close", () => opener.focus(), { once: true });
      }
      return;
    }
    const closer = target?.closest<HTMLElement>("[data-dialog-close]");
    closer?.closest("dialog")?.close();
  });
}
