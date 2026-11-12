// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Review threads show a Reply button that opens the reply form in place
// (`details.thread-reply-box`). A reply the browser restored (back,
// reload) opens its form again, so a started reply is never hidden.

function openStarted(root: ParentNode): void {
  root.querySelectorAll<HTMLDetailsElement>("details.thread-reply-box").forEach((d) => {
    const text = d.querySelector<HTMLTextAreaElement>("textarea");
    if (text && text.value.trim() !== "") d.open = true;
  });
}

export function installReplies(): void {
  // Restored form values are filled in by the time the page shows.
  window.addEventListener("pageshow", () => openStarted(document));
  openStarted(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) openStarted(e.target);
  });
  // Opening a reply box puts the cursor in it.
  document.addEventListener(
    "toggle",
    (e) => {
      const d = e.target;
      if (d instanceof HTMLDetailsElement && d.classList.contains("thread-reply-box") && d.open) {
        d.querySelector<HTMLTextAreaElement>("textarea")?.focus();
      }
    },
    true,
  );
}
