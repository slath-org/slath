// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The <slath-editor> shell around a Markdown <textarea>. The textarea stays
// the form field and works without JavaScript; this adds a toolbar
// (role=toolbar, roving tabindex), shortcuts with a help dialog, Write and
// Preview tabs (the preview is the server's renderer), a rich-text and a
// source mode (each loaded on demand), reference suggestions, attachment
// upload by button, paste, and drop, short references for pasted links to
// this site, and drafts kept in this browser until the form is sent.
//
// Text the person didn't change is submitted exactly as it was: the editor
// only writes back into the textarea after an edit.
import { csrfToken } from "../csrf";
import { attachCombobox, type Combobox } from "./combobox";
import { clearDraft, loadDraft, saveDraft } from "./drafts";
import { trustedHTML } from "./tt";
import type { Command, Mode } from "./mode";

interface Action {
  command: Command;
  label: string;
  /** Shown in the help dialog and as aria-keyshortcuts. */
  keys: string;
  icon: string;
  match: (e: KeyboardEvent) => boolean;
}

const mod = (e: KeyboardEvent) => e.ctrlKey || e.metaKey;
const ACTIONS: Action[] = [
  { command: "bold", label: "Bold", keys: "Control+B", icon: "bold", match: (e) => mod(e) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "b" },
  { command: "italic", label: "Italic", keys: "Control+I", icon: "italic", match: (e) => mod(e) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "i" },
  { command: "code", label: "Inline code", keys: "Control+E", icon: "code", match: (e) => mod(e) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "e" },
  { command: "link", label: "Link", keys: "Control+K", icon: "link", match: (e) => mod(e) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "k" },
  { command: "heading", label: "Heading", keys: "Control+Alt+2", icon: "heading", match: (e) => mod(e) && e.altKey && e.code === "Digit2" },
  { command: "quote", label: "Quote", keys: "Control+Shift+9", icon: "text-quote", match: (e) => mod(e) && e.shiftKey && e.code === "Digit9" },
  { command: "bullet", label: "Bulleted list", keys: "Control+Shift+8", icon: "list", match: (e) => mod(e) && e.shiftKey && e.code === "Digit8" },
  { command: "ordered", label: "Numbered list", keys: "Control+Shift+7", icon: "list-ordered", match: (e) => mod(e) && e.shiftKey && e.code === "Digit7" },
  { command: "task", label: "Task list", keys: "Control+Shift+X", icon: "list-todo", match: (e) => mod(e) && e.shiftKey && e.key.toLowerCase() === "x" },
  { command: "codeblock", label: "Code block", keys: "Control+Alt+C", icon: "square-code", match: (e) => mod(e) && e.altKey && e.code === "KeyC" },
  { command: "table", label: "Table", keys: "Control+Alt+T", icon: "table", match: (e) => mod(e) && e.altKey && e.code === "KeyT" },
];

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

let ids = 0;

/** Short reference for a pasted link to this site, if it is one. */
export function shortReference(text: string, repo: string): string | null {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return null;
  }
  if (url.origin !== window.location.origin || url.search || url.hash) return null;
  const m = /^\/([\w.-]+)\/([\w.-]+)\/(issues|commit)\/([0-9a-f]{7,40}|\d+)$/.exec(url.pathname);
  if (!m) return null;
  const [, ns, name, kind, id] = m;
  const here = `${ns}/${name}`.toLowerCase() === repo.toLowerCase();
  if (kind === "issues") return here ? `#${id}` : `${ns}/${name}#${id}`;
  const sha = id.slice(0, 12);
  return here ? sha : `${ns}/${name}@${sha}`;
}

export async function mount(host: HTMLElement): Promise<void> {
  if (host.dataset.ready) return;
  host.dataset.ready = "1";
  const textarea = host.querySelector("textarea");
  if (!textarea) return;
  const form = textarea.form;
  const repo = host.dataset.repo ?? "";
  const item = host.dataset.item ?? "";
  const uploadUrl = host.dataset.upload ?? "";
  const original = textarea.value;
  const required = textarea.required;
  const labelText =
    (textarea.id && document.querySelector(`label[for="${textarea.id}"]`)?.textContent?.trim()) || "Text";
  const n = ++ids;

  form?.querySelectorAll<HTMLElement>("[data-editor-nojs]").forEach((b) => (b.hidden = true));
  host.classList.add("editor");

  // Tabs.
  const bar = el("div", "editor-bar");
  const tabs = el("div", "editor-tabs");
  tabs.setAttribute("role", "tablist");
  tabs.setAttribute("aria-label", `${labelText}: view`);
  const writeTab = el("button", "editor-tab", "Write");
  const previewTab = el("button", "editor-tab", "Preview");
  for (const [t, panel] of [
    [writeTab, `editor-${n}-write`],
    [previewTab, `editor-${n}-preview`],
  ] as const) {
    t.type = "button";
    t.setAttribute("role", "tab");
    t.setAttribute("aria-controls", panel);
    t.id = `${panel}-tab`;
  }
  tabs.append(writeTab, previewTab);

  // Toolbar with roving tabindex.
  const toolbar = el("div", "editor-toolbar");
  toolbar.setAttribute("role", "toolbar");
  toolbar.setAttribute("aria-label", "Formatting");
  const buttons: HTMLButtonElement[] = [];
  const sprite = host.dataset.icons ?? "";
  const addButton = (label: string, icon: string, keys: string | null, run: () => void) => {
    const b = el("button", "editor-tool");
    b.type = "button";
    b.setAttribute("aria-label", label);
    b.title = keys ? `${label} (${keys.replace("Control", "Ctrl")})` : label;
    if (keys) b.setAttribute("aria-keyshortcuts", keys);
    const svgNs = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(svgNs, "svg");
    svg.setAttribute("class", "icon");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    const use = document.createElementNS(svgNs, "use");
    use.setAttribute("href", `${sprite}#${icon}`);
    svg.append(use);
    b.append(svg);
    b.tabIndex = buttons.length === 0 ? 0 : -1;
    // Keep the focus and selection in the editor. Not for touch: WebKit
    // then drops the click; the editors keep their selection anyway.
    b.addEventListener("pointerdown", (e) => {
      if (e.pointerType !== "touch") e.preventDefault();
    });
    b.addEventListener("mousedown", (e) => e.preventDefault());
    b.addEventListener("click", run);
    buttons.push(b);
    toolbar.append(b);
    return b;
  };
  toolbar.addEventListener("keydown", (e) => {
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (i < 0) return;
    let next = -1;
    if (e.key === "ArrowRight") next = (i + 1) % buttons.length;
    else if (e.key === "ArrowLeft") next = (i - 1 + buttons.length) % buttons.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = buttons.length - 1;
    if (next < 0) return;
    e.preventDefault();
    buttons[i].tabIndex = -1;
    buttons[next].tabIndex = 0;
    buttons[next].focus();
  });

  const modeButton = el("button", "button button-quiet editor-mode");
  modeButton.type = "button";
  modeButton.textContent = host.dataset.mode === "source" ? "Rich text" : "Markdown";
  const helpButton = el("button", "button button-quiet editor-help-button", "Shortcuts");
  helpButton.type = "button";
  bar.append(tabs, toolbar, modeButton, helpButton);

  const writePanel = el("div", "editor-write");
  writePanel.id = `editor-${n}-write`;
  writePanel.setAttribute("role", "tabpanel");
  writePanel.setAttribute("aria-labelledby", `${writePanel.id}-tab`);
  const surface = el("div", "editor-surface");
  writePanel.append(surface);
  const preview = el("div", "editor-preview markdown");
  preview.id = `editor-${n}-preview`;
  preview.setAttribute("role", "tabpanel");
  preview.setAttribute("aria-labelledby", `${preview.id}-tab`);
  preview.tabIndex = 0;
  preview.hidden = true;
  const status = el("div", "editor-status");
  status.setAttribute("role", "status");
  const notice = el("p", "editor-notice hint");
  notice.hidden = true;
  const fileInput = el("input");
  fileInput.type = "file";
  fileInput.multiple = true;
  fileInput.hidden = true;

  // Help dialog.
  const help = el("dialog", "dialog editor-help");
  help.setAttribute("aria-labelledby", `editor-${n}-help-title`);
  const helpTitle = el("h2", undefined, "Editor shortcuts");
  helpTitle.id = `editor-${n}-help-title`;
  const table = el("table", "table");
  const tbody = el("tbody");
  const row = (what: string, keys: string) => {
    const tr = el("tr");
    tr.append(el("th", undefined, what), el("td", undefined, keys.replace(/Control/g, "Ctrl")));
    tbody.append(tr);
  };
  for (const a of ACTIONS) row(a.label, a.keys);
  row("Suggestions", "@ people, # issues, ~ labels, % milestones; ↑ ↓ to choose, Enter or Tab to insert, Esc to close");
  row("Leave the editor", "Tab");
  table.append(tbody);
  const closeHelp = el("button", "button", "Close");
  closeHelp.type = "button";
  closeHelp.addEventListener("click", () => help.close());
  help.append(helpTitle, table, closeHelp);
  helpButton.addEventListener("click", () => {
    help.showModal();
    help.addEventListener("close", () => helpButton.focus(), { once: true });
  });

  textarea.hidden = true;
  textarea.required = false;
  host.prepend(bar);
  host.append(writePanel, preview, notice, status, fileInput, help);

  let mode: Mode | null = null;
  let combo: Combobox | null = null;
  let dirty = false;
  let submitted = false;
  const draftKey = `${window.location.pathname}:${textarea.name}:${textarea.id}`;

  const current = () => (mode && dirty ? mode.getMarkdown() : textarea.value);
  const sync = () => {
    if (mode && dirty) textarea.value = mode.getMarkdown();
  };
  let draftTimer: number | undefined;
  /** The text the current mode was opened with. */
  let loaded = original;
  const changed = () => {
    // Editors may report changes that aren't edits (DOM normalization);
    // only a different document counts.
    if (!dirty && mode && mode.getMarkdown().trimEnd() === loaded.trimEnd()) return;
    dirty = true;
    combo?.update();
    window.clearTimeout(draftTimer);
    draftTimer = window.setTimeout(() => {
      sync();
      saveDraft(draftKey, textarea.value);
    }, 400);
  };

  const load = async (kind: "wysiwyg" | "source", markdown: string): Promise<Mode> => {
    const label = `${labelText} (${kind === "source" ? "Markdown source" : "rich text"})`;
    const m =
      kind === "source"
        ? await (await import("./source")).mount({ host: surface, markdown, label })
        : await (await import("./wysiwyg")).mount({
            host: surface,
            markdown,
            label,
            externalImages: host.dataset.externalImages === "true",
          });
    return m;
  };

  const show = async (kind: "wysiwyg" | "source", markdown: string, announce: boolean) => {
    combo?.destroy();
    mode?.destroy();
    host.setAttribute("aria-busy", "true");
    let m = await load(kind, markdown);
    notice.hidden = true;
    // Text the rich editor would rewrite opens as source.
    if (m.kind === "wysiwyg" && markdown.trim() !== "" && m.getMarkdown().trimEnd() !== markdown.trimEnd()) {
      m.destroy();
      m = await load("source", markdown);
      notice.textContent =
        "This text uses Markdown the rich text editor would rewrite, so it is shown as Markdown source.";
      notice.hidden = false;
    }
    mode = m;
    loaded = markdown;
    host.removeAttribute("aria-busy");
    m.onChange(changed);
    combo = attachCombobox(m, host, repo, item, status);
    modeButton.textContent = m.kind === "source" ? "Rich text" : "Markdown";
    modeButton.setAttribute("aria-label", m.kind === "source" ? "Switch to rich text" : "Switch to Markdown source");
    if (announce) {
      status.textContent = m.kind === "source" ? "Markdown source mode" : "Rich text mode";
      m.focus();
    }
  };

  for (const a of ACTIONS) addButton(a.label, a.icon, a.keys, () => mode?.run(a.command));
  if (uploadUrl) {
    addButton("Attach files", "paperclip", null, () => fileInput.click());
  }

  modeButton.addEventListener("click", async () => {
    const next = mode?.kind === "source" ? "wysiwyg" : "source";
    const text = current();
    await show(next, text, true);
    const body = new URLSearchParams({ mode: next });
    void fetch("/-/settings/editor-mode", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/x-www-form-urlencoded", "X-CSRF-Token": csrfToken() ?? "" },
      body,
    }).catch(() => {});
  });

  // Write/Preview.
  const select = async (which: "write" | "preview") => {
    const write = which === "write";
    writeTab.setAttribute("aria-selected", String(write));
    previewTab.setAttribute("aria-selected", String(!write));
    writeTab.tabIndex = write ? 0 : -1;
    previewTab.tabIndex = write ? -1 : 0;
    writePanel.hidden = !write;
    preview.hidden = write;
    toolbar.hidden = !write;
    if (write) {
      mode?.focus();
      return;
    }
    preview.textContent = "Loading the preview…";
    try {
      const params = new URLSearchParams({ repo, field: "text" });
      const r = await fetch(`/-/ui/markdown/preview?${params}`, {
        method: "POST",
        credentials: "same-origin",
        headers: {
          "content-type": "application/x-www-form-urlencoded",
          "X-CSRF-Token": csrfToken() ?? "",
          "HX-Request": "true",
        },
        body: new URLSearchParams({ text: current() }),
      });
      if (!r.ok) throw new Error(String(r.status));
      preview.innerHTML = trustedHTML(await r.text());
    } catch {
      preview.textContent = "The preview couldn't be loaded. Check your connection and try again.";
    }
  };
  writeTab.addEventListener("click", () => void select("write"));
  previewTab.addEventListener("click", () => void select("preview"));
  tabs.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const toPreview = document.activeElement === writeTab;
    (toPreview ? previewTab : writeTab).focus();
    void select(toPreview ? "preview" : "write");
  });

  // Shortcuts, and Tab leaving the editor (no keyboard trap in tables).
  host.addEventListener(
    "keydown",
    (e) => {
      if (!mode || preview.hidden === false) return;
      const action = ACTIONS.find((a) => a.match(e));
      if (action && host.contains(document.activeElement)) {
        e.preventDefault();
        e.stopPropagation();
        mode.run(action.command);
        return;
      }
      if (e.key === "Tab" && !e.ctrlKey && !e.altKey && !e.metaKey) {
        // The browser moves focus; the editing mode must not take the key.
        e.stopPropagation();
      }
    },
    true,
  );

  // Uploads: button, paste, and drop.
  const upload = (file: File) => {
    if (!uploadUrl) return;
    const xhr = new XMLHttpRequest();
    const line = el("p", "editor-upload");
    const text = el("span", undefined, `Uploading ${file.name}…`);
    const cancel = el("button", "button button-quiet", "Cancel");
    cancel.type = "button";
    cancel.setAttribute("aria-label", `Cancel uploading ${file.name}`);
    cancel.addEventListener("click", () => xhr.abort());
    line.append(text, " ", cancel);
    status.before(line);
    xhr.upload.addEventListener("progress", (e) => {
      if (e.lengthComputable) text.textContent = `Uploading ${file.name}… ${Math.round((100 * e.loaded) / e.total)}%`;
    });
    xhr.addEventListener("loadend", () => {
      line.remove();
      if (xhr.status === 201) {
        const j = JSON.parse(xhr.responseText) as { markdown: string };
        mode?.insertMarkdown(j.markdown);
        changed();
        status.textContent = `Attached ${file.name}`;
      } else if (xhr.status === 0) {
        status.textContent = `Upload of ${file.name} canceled`;
      } else {
        let why = "";
        try {
          why = (JSON.parse(xhr.responseText) as { detail?: string }).detail ?? "";
        } catch {
          why = "";
        }
        status.textContent = `${file.name} couldn't be attached. ${why}`.trim();
      }
    });
    xhr.open("POST", uploadUrl);
    xhr.setRequestHeader("X-CSRF-Token", csrfToken() ?? "");
    const fd = new FormData();
    fd.append("file", file, file.name);
    xhr.send(fd);
  };
  fileInput.addEventListener("change", () => {
    for (const f of Array.from(fileInput.files ?? [])) upload(f);
    fileInput.value = "";
  });
  host.addEventListener(
    "paste",
    (e) => {
      const data = e.clipboardData;
      if (!data || !mode) return;
      const files = Array.from(data.files);
      if (files.length > 0 && uploadUrl) {
        e.preventDefault();
        e.stopPropagation();
        files.forEach(upload);
        return;
      }
      const short = shortReference(data.getData("text/plain"), repo);
      if (short) {
        e.preventDefault();
        e.stopPropagation();
        mode.replaceBeforeCaret(0, short);
        changed();
      }
    },
    true,
  );
  host.addEventListener("dragover", (e) => {
    if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
  });
  host.addEventListener(
    "drop",
    (e) => {
      const files = Array.from(e.dataTransfer?.files ?? []);
      if (files.length === 0 || !uploadUrl) return;
      e.preventDefault();
      e.stopPropagation();
      files.forEach(upload);
    },
    true,
  );

  // Sending the form: write back edits, check required, drop the draft.
  form?.addEventListener("submit", (e) => {
    sync();
    if (required && textarea.value.trim() === "") {
      e.preventDefault();
      status.textContent = `${labelText} is required.`;
      mode?.focus();
      return;
    }
    submitted = true;
    clearDraft(draftKey);
  });

  // Leaving the page keeps the latest text as a draft.
  window.addEventListener("pagehide", () => {
    if (dirty && !submitted) {
      sync();
      saveDraft(draftKey, textarea.value);
    }
  });

  // A saved draft wins over the page's text, with a way back.
  const draft = loadDraft(draftKey);
  const start = draft !== null && draft !== original ? draft : original;
  await select("write").catch(() => {});
  const kind = host.dataset.mode === "source" ? "source" : "wysiwyg";
  await show(kind, start, false);
  if (start !== original) {
    dirty = true;
    textarea.value = start;
    const p = el("p", "editor-draft hint", "Restored text you didn't send yet. ");
    const discard = el("button", "button button-quiet", "Discard it");
    discard.type = "button";
    discard.addEventListener("click", async () => {
      clearDraft(draftKey);
      textarea.value = original;
      dirty = false;
      await show(mode?.kind ?? kind, original, true);
      p.remove();
    });
    p.append(discard);
    host.insertBefore(p, writePanel);
  }
  (host as HTMLElement & { slathEditor?: unknown }).slathEditor = {
    /** For tests: parse and serialize in the current mode. */
    async roundtrip(md: string): Promise<string> {
      const probe = el("div");
      probe.hidden = true;
      document.body.append(probe);
      const m = await (await import("./wysiwyg")).mount({ host: probe, markdown: md, label: "probe" });
      // (No images load: external images are off for the probe.)
      const out = m.getMarkdown();
      m.destroy();
      probe.remove();
      return out;
    },
    markdown: () => current(),
    mode: () => mode?.kind,
  };
  host.dataset.state = "ready";
  host.dispatchEvent(new CustomEvent("slath-editor:ready", { bubbles: true }));
}
