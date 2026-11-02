// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Source mode: CodeMirror 6 with Markdown highlighting, mounted in a shadow
// root so its styles are adopted stylesheets (no inline <style>, which the
// CSP forbids). The document is the Markdown text, so nothing is rewritten.
// Loaded only when someone uses source mode, in its own chunk.
import { EditorSelection, EditorState } from "@codemirror/state";
import { EditorView, drawSelection, keymap } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { HighlightStyle, LanguageSupport, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";
import {
  deleteMarkupBackward,
  insertNewlineContinueMarkup,
  markdownLanguage,
} from "@codemirror/lang-markdown";
import type { Command, Mode, MountOptions } from "./mode";

const highlight = HighlightStyle.define([
  { tag: tags.heading, fontWeight: "600" },
  { tag: tags.strong, fontWeight: "600" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: [tags.link, tags.url], color: "var(--link)" },
  { tag: [tags.monospace, tags.processingInstruction], color: "var(--text-secondary)" },
  { tag: tags.quote, color: "var(--text-secondary)" },
]);

const theme = EditorView.theme({
  "&": {
    color: "var(--text)",
    backgroundColor: "var(--surface)",
    fontFamily: "var(--font-mono)",
    fontSize: "var(--code-size)",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-content": { padding: "var(--space-2) var(--space-3)", minHeight: "10rem", caretColor: "var(--text)" },
  ".cm-scroller": { fontFamily: "inherit", lineHeight: "1.6" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--selection)",
  },
});

/** Styles for the suggestion list, which lives in the same shadow root. */
const POPUP_CSS = `
.ac-list { position: absolute; z-index: 10; margin: 0; padding: var(--space-1) 0; list-style: none;
  min-inline-size: 16rem; max-inline-size: 28rem; background: var(--surface); color: var(--text);
  border: 1px solid var(--control-border); border-radius: var(--radius-md); box-shadow: var(--shadow-overlay);
  font-family: var(--font-ui); font-size: var(--text-sm); }
.ac-option { display: flex; gap: 0.75rem; align-items: center; min-block-size: var(--target-min); padding: 0 var(--space-3); cursor: pointer; }
.ac-option[aria-selected="true"] { background: var(--selection); }
.ac-detail { color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
`;

function wrap(view: EditorView, before: string, after = before, placeholder = "text") {
  view.dispatch(
    view.state.changeByRange((r) => {
      const inner = r.empty ? placeholder : view.state.sliceDoc(r.from, r.to);
      return {
        changes: { from: r.from, to: r.to, insert: `${before}${inner}${after}` },
        range: EditorSelection.range(r.from + before.length, r.from + before.length + inner.length),
      };
    }),
  );
}

/** Puts `prefix` at the start of every selected line. */
function prefixLines(view: EditorView, prefix: (i: number) => string) {
  const { state } = view;
  const changes = [];
  const sel = state.selection.main;
  let i = 0;
  for (let pos = state.doc.lineAt(sel.from).from; pos <= sel.to; ) {
    const line = state.doc.lineAt(pos);
    changes.push({ from: line.from, insert: prefix(i++) });
    if (line.to + 1 > state.doc.length) break;
    pos = line.to + 1;
  }
  view.dispatch({ changes });
}

export async function mount(opts: MountOptions): Promise<Mode> {
  const holder = document.createElement("div");
  holder.className = "editor-source";
  opts.host.append(holder);
  const shadow = holder.attachShadow({ mode: "open" });
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(POPUP_CSS);
  shadow.adoptedStyleSheets = [sheet];
  let changed = () => {};
  const view = new EditorView({
    root: shadow,
    parent: shadow,
    state: EditorState.create({
      doc: opts.markdown,
      extensions: [
        history(),
        drawSelection(),
        EditorView.lineWrapping,
        new LanguageSupport(markdownLanguage),
        syntaxHighlighting(highlight),
        theme,
        // Tab stays with the browser, so the editor is no keyboard trap.
        keymap.of([
          { key: "Enter", run: insertNewlineContinueMarkup },
          { key: "Backspace", run: deleteMarkupBackward },
          ...defaultKeymap.filter((b) => b.key !== "Tab" && b.key !== "Shift-Tab"),
          ...historyKeymap,
        ]),
        EditorView.contentAttributes.of({
          "aria-label": opts.label,
          "aria-multiline": "true",
          role: "textbox",
        }),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) changed();
        }),
      ],
    }),
  });

  const run = (command: Command) => {
    switch (command) {
      case "bold":
        wrap(view, "**");
        break;
      case "italic":
        wrap(view, "*");
        break;
      case "code":
        wrap(view, "`", "`", "code");
        break;
      case "link":
        wrap(view, "[", "](https://)", "text");
        break;
      case "heading":
        prefixLines(view, () => "## ");
        break;
      case "quote":
        prefixLines(view, () => "> ");
        break;
      case "bullet":
        prefixLines(view, () => "- ");
        break;
      case "ordered":
        prefixLines(view, (i) => `${i + 1}. `);
        break;
      case "task":
        prefixLines(view, () => "- [ ] ");
        break;
      case "codeblock":
        wrap(view, "```\n", "\n```", "code");
        break;
      case "table":
        view.dispatch(view.state.replaceSelection("\n| Column | Column |\n| - | - |\n| | |\n"));
        break;
    }
    view.focus();
  };

  return {
    kind: "source",
    root: view.contentDOM,
    popupParent: shadow,
    anchor: holder,
    getMarkdown: () => view.state.doc.toString(),
    setMarkdown: (md) => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: md } }),
    focus: () => view.focus(),
    run,
    insertMarkdown(md) {
      view.dispatch(view.state.replaceSelection(md));
      view.focus();
    },
    textBeforeCaret() {
      const sel = view.state.selection.main;
      if (!sel.empty) return null;
      const line = view.state.doc.lineAt(sel.head);
      return line.text.slice(0, sel.head - line.from);
    },
    replaceBeforeCaret(n, text) {
      const at = view.state.selection.main.head;
      view.dispatch({
        changes: { from: at - n, to: at, insert: text },
        selection: { anchor: at - n + text.length },
      });
      view.focus();
    },
    caretRect() {
      const c = view.coordsAtPos(view.state.selection.main.head);
      return c ? { left: c.left, bottom: c.bottom } : null;
    },
    onChange(cb) {
      changed = cb;
    },
    destroy() {
      view.destroy();
      holder.remove();
    },
  };
}
