// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The list of keyboard shortcuts (dialog#shortcuts, in every page): ? opens
// it while focus isn't in a field, unless single-key shortcuts are off.
// The footer's "Keyboard shortcuts" button opens it too (a Weave dialog
// button, see dialog.ts).

import { openDialog } from "./dialog";
import { singleKey } from "./keys";

export function installShortcutList(): void {
  document.addEventListener("keydown", (e) => {
    if (e.key !== "?" || e.defaultPrevented || !singleKey(e) || document.querySelector("dialog[open]")) return;
    e.preventDefault();
    openDialog("shortcuts", document.activeElement);
  });
}
