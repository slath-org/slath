// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Dialogs open and close through invoker commands in the markup, without
// script: `<button commandfor="ID" command="show-modal">` opens the
// <dialog> as modal (focus moves into it, Escape closes it, and focus
// returns to the button), and `command="close"` closes it. Browsers without
// invoker commands get the polyfill, loaded only there.

export function installDialogs(): void {
  if (!("commandForElement" in HTMLButtonElement.prototype)) {
    void import("invokers-polyfill");
  }
}
