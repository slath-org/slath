// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Navigation inside the page frame. The frame boosts its links and forms
// (base.html): htmx fetches the same URL a normal click would and swaps
// the frame, so the top bar stays, and back, reload, and shared links give
// the same page. This module keeps that to navigation (links and GET forms;
// POST forms stay ordinary submissions) and starts a new page at its top.

const ACTIONS = ["hx-get", "hx-post", "hx-put", "hx-patch", "hx-delete", "hx-action"];

interface SwapDetail {
  ctx?: { target?: Element | null; request?: { anchor?: string } };
}

export function installNavigation(): void {
  document.addEventListener("htmx:before:init", (evt) => {
    const elt = evt.target;
    if (
      elt instanceof HTMLFormElement &&
      elt.method !== "get" &&
      !ACTIONS.some((a) => elt.hasAttribute(a))
    ) {
      evt.preventDefault();
    }
  });
  document.addEventListener("htmx:after:swap", (evt) => {
    const ctx = (evt as CustomEvent<SwapDetail>).detail?.ctx;
    if (ctx?.target?.id !== "app-frame" || ctx.request?.anchor) return;
    window.scrollTo(0, 0);
  });
}
