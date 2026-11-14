// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Two helpers for fields, both added only with scripts:
// - Password fields get a button that shows and hides the text.
// - Fields with a length limit show how many characters are left once the
//   text comes within a fifth of the limit, announced politely.

let seq = 0;

function reveal(input: HTMLInputElement): void {
  const wrap = document.createElement("div");
  wrap.className = "input-with-button";
  input.before(wrap);
  wrap.append(input);
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "button";
  // A toggle with a constant name: pressed shows the text.
  btn.textContent = "Show";
  btn.setAttribute("aria-pressed", "false");
  if (!input.id) input.id = `password-${++seq}`;
  btn.setAttribute("aria-controls", input.id);
  btn.addEventListener("click", () => {
    const show = input.type === "password";
    input.type = show ? "text" : "password";
    btn.setAttribute("aria-pressed", String(show));
    input.focus();
  });
  wrap.append(btn);
  // The form never sends a password in the clear in browser history.
  input.form?.addEventListener("submit", () => {
    input.type = "password";
  });
}

function counter(field: HTMLInputElement | HTMLTextAreaElement): void {
  const max = field.maxLength;
  if (max <= 0) return;
  const out = document.createElement("p");
  out.className = "hint field-count";
  out.id = `count-${++seq}`;
  out.setAttribute("aria-live", "polite");
  out.hidden = true;
  field.after(out);
  const desc = field.getAttribute("aria-describedby");
  field.setAttribute("aria-describedby", desc ? `${desc} ${out.id}` : out.id);
  const update = () => {
    const left = max - field.value.length;
    const near = left <= Math.ceil(max / 5);
    out.hidden = !near;
    if (near) out.textContent = left === 1 ? "1 character left" : `${left} characters left`;
  };
  field.addEventListener("input", update);
  update();
}

export function installFieldHelpers(): void {
  const run = (root: ParentNode) => {
    root.querySelectorAll<HTMLInputElement>("input[type=password]:not([data-reveal-ready])").forEach((i) => {
      i.dataset.revealReady = "";
      reveal(i);
    });
    root
      .querySelectorAll<HTMLInputElement | HTMLTextAreaElement>(
        "input[maxlength]:not([readonly]):not([data-count-ready]), textarea[maxlength]:not([data-count-ready])",
      )
      .forEach((f) => {
        f.dataset.countReady = "";
        counter(f);
      });
  };
  run(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) run(e.target);
  });
}
