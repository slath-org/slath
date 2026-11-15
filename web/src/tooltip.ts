// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Tooltips: elements with `data-tip="text"` show it on hover, on keyboard
// focus, and on a tap, close it with Escape, a tap elsewhere, or leaving,
// and follow their element when the page scrolls. One tooltip element serves the page. Where the text adds to
// the element's name (an exact time, a full hash), screen readers get it
// as the element's description; a tooltip that only repeats the name of an
// icon-only control stays visual, since the name is already there.

// The id is Slath's own, so it can't meet an id the page uses (the
// catalog's Tooltip entry is `#tooltip`).
const ID = "slath-tooltip";

let tip: HTMLElement | null = null;
let owner: HTMLElement | null = null;

function element(): HTMLElement {
  // Going back swaps the whole body for the page's HTML, which takes the
  // tooltip out: use the page's own, or a new one.
  if (!tip?.isConnected) {
    tip = document.getElementById(ID);
  }
  if (!tip) {
    tip = document.createElement("div");
    tip.id = ID;
    tip.className = "tooltip";
    tip.setAttribute("role", "tooltip");
    tip.hidden = true;
    document.body.append(tip);
  }
  return tip;
}

function hide(): void {
  if (owner?.getAttribute("aria-describedby") === ID) owner.removeAttribute("aria-describedby");
  owner = null;
  if (tip) tip.hidden = true;
}

function show(el: HTMLElement): void {
  const text = el.dataset.tip;
  if (!text) return;
  const t = element();
  t.textContent = text;
  t.hidden = false;
  owner = el;
  const name = (el.getAttribute("aria-label") ?? el.textContent ?? "").trim();
  if (!name.includes(text) && !el.hasAttribute("aria-describedby")) el.setAttribute("aria-describedby", ID);
  // Below the element, or above it where there's no room; inside the screen.
  const r = el.getBoundingClientRect();
  const w = t.offsetWidth;
  const h = t.offsetHeight;
  const gap = 6;
  const vw = document.documentElement.clientWidth;
  const left = Math.min(Math.max(8, r.left + r.width / 2 - w / 2), vw - w - 8);
  const below = r.bottom + gap + h <= window.innerHeight;
  t.style.left = `${left}px`;
  t.style.top = `${below ? r.bottom + gap : r.top - gap - h}px`;
}

function target(e: Event): HTMLElement | null {
  return e.target instanceof Element ? e.target.closest<HTMLElement>("[data-tip]") : null;
}

export function installTooltips(): void {
  document.addEventListener("pointerover", (e) => {
    if ((e as PointerEvent).pointerType !== "mouse") return;
    const el = target(e);
    if (el && el !== owner) show(el);
  });
  document.addEventListener("pointerout", (e) => {
    if ((e as PointerEvent).pointerType !== "mouse") return;
    const el = target(e);
    if (el && el === owner && !el.contains((e as PointerEvent).relatedTarget as Node | null)) hide();
  });
  document.addEventListener("focusin", (e) => {
    const el = target(e);
    if (el) show(el);
  });
  document.addEventListener("focusout", (e) => {
    if (target(e) === owner) hide();
  });
  // A tap shows it; a second tap, or a tap elsewhere, hides it.
  document.addEventListener("pointerup", (e) => {
    if (e.pointerType === "mouse") return;
    const el = target(e);
    if (el && el !== owner) show(el);
    else hide();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && owner) hide();
  });
  // Focus scrolls its element into view: keep the tooltip with it, and
  // close it once the element has left the screen.
  window.addEventListener(
    "scroll",
    () => {
      if (!owner) return;
      const r = owner.getBoundingClientRect();
      if (r.bottom < 0 || r.top > window.innerHeight) hide();
      else show(owner);
    },
    { passive: true, capture: true },
  );
  document.addEventListener("htmx:before:swap", hide);
}
