// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// What every single-key shortcut (j, n, ?) checks first: no Ctrl, Cmd, or
// Alt, focus not in a field, and the viewer hasn't turned them off
// (data-single-keys="off" on the root, from their setting or cookie;
// WCAG 2.1.4). Shortcuts with Ctrl or Cmd don't depend on the setting.

/** Focus is in a field, where keys type text. */
export function editing(t: EventTarget | null): boolean {
  return (
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable)
  );
}

/** Single-key shortcuts are turned on for this viewer. */
export function singleKeysOn(): boolean {
  return document.documentElement.dataset.singleKeys !== "off";
}

/** The key press may act as a single-key shortcut. */
export function singleKey(e: KeyboardEvent): boolean {
  return !e.ctrlKey && !e.metaKey && !e.altKey && !e.isComposing && singleKeysOn() && !editing(e.target);
}
