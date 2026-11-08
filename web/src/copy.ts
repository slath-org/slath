// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Copy buttons: `<button data-copy="ID" hidden>` copies the text of the
// element with that id. The buttons stay hidden without this script (and
// without a clipboard), since the text can always be selected.
import { t } from "./i18n/en";
import { showToast } from "./toast";

export function installCopyButtons(): void {
  if (!navigator.clipboard) return;
  const reveal = (root: ParentNode) => {
    root
      .querySelectorAll<HTMLButtonElement>("button[data-copy]")
      .forEach((b) => {
        b.hidden = false;
      });
  };
  reveal(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) reveal(e.target);
  });
  document.addEventListener("click", (e) => {
    const btn =
      e.target instanceof Element
        ? e.target.closest<HTMLButtonElement>("button[data-copy]")
        : null;
    if (!btn) return;
    const src = document.getElementById(btn.dataset.copy ?? "");
    if (!src) return;
    navigator.clipboard.writeText((src.textContent ?? "").trim()).then(
      () => showToast(t("copied"), undefined, "info"),
      () => showToast(t("copyFailed")),
    );
  });
}
