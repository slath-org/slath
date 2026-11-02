// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Rich-text mode: Milkdown (ProseMirror with remark/mdast) with the
// CommonMark and GFM presets. Markdown is parsed and serialized by remark;
// serializer options follow the style the server renderer and the round-trip
// corpus use. Loaded only on pages with an editor, in its own chunk.
import {
  Editor,
  defaultValueCtx,
  editorViewCtx,
  editorViewOptionsCtx,
  remarkStringifyOptionsCtx,
  rootCtx,
} from "@milkdown/kit/core";
import {
  commonmark,
  createCodeBlockCommand,
  imageSchema,
  toggleEmphasisCommand,
  toggleInlineCodeCommand,
  toggleStrongCommand,
  wrapInBlockquoteCommand,
  wrapInBulletListCommand,
  wrapInHeadingCommand,
  wrapInOrderedListCommand,
} from "@milkdown/kit/preset/commonmark";
import { gfm, insertTableCommand, remarkGFMPlugin } from "@milkdown/kit/preset/gfm";
import { history } from "@milkdown/kit/plugin/history";
import { listener, listenerCtx } from "@milkdown/kit/plugin/listener";
import { TextSelection } from "@milkdown/kit/prose/state";
import { $view, callCommand, getMarkdown, insert, replaceAll } from "@milkdown/kit/utils";
import type { Command, Mode, MountOptions } from "./mode";

/** Whether an image source is on this site (relative or same origin). */
function local(src: string): boolean {
  try {
    return new URL(src, window.location.href).origin === window.location.origin;
  } catch {
    return false;
  }
}

/**
 * Images while editing: images from elsewhere are shown only where the
 * instance allows external images in user content; otherwise a text
 * placeholder stands in, so editing never makes requests to other sites.
 */
const imageView = (external: boolean) =>
  $view(imageSchema.node, () => (node) => {
    const src = String(node.attrs.src ?? "");
    const alt = String(node.attrs.alt ?? "");
    if (external || local(src)) {
      const img = document.createElement("img");
      img.alt = alt;
      if (node.attrs.title) img.title = String(node.attrs.title);
      img.src = src;
      return { dom: img };
    }
    const span = document.createElement("span");
    span.className = "editor-image-placeholder";
    span.textContent = `[image: ${alt || src}]`;
    return { dom: span };
  });

export async function mount(opts: MountOptions): Promise<Mode> {
  const surface = document.createElement("div");
  surface.className = "editor-rich markdown";
  opts.host.append(surface);
  let changed = () => {};
  let ready = false;
  const editor = await Editor.make()
    .config((ctx) => {
      ctx.set(rootCtx, surface);
      ctx.set(defaultValueCtx, opts.markdown);
      ctx.update(editorViewOptionsCtx, (prev) => ({
        ...prev,
        attributes: {
          "aria-label": opts.label,
          role: "textbox",
          "aria-multiline": "true",
          class: "editor-content",
        },
      }));
      ctx.update(remarkStringifyOptionsCtx, (prev) => ({
        ...prev,
        bullet: "-" as const,
        emphasis: "*" as const,
        strong: "*" as const,
        rule: "-" as const,
        fence: "`" as const,
      }));
      ctx.set(remarkGFMPlugin.options.key, { tablePipeAlign: false });
      ctx.get(listenerCtx).updated(() => {
        if (ready) changed();
      });
    })
    .use(commonmark)
    .use(gfm)
    .use(history)
    .use(listener)
    .use(imageView(opts.externalImages ?? false))
    .create();
  ready = true;
  const view = () => editor.ctx.get(editorViewCtx);
  const dom = view().dom as HTMLElement;

  const run = (command: Command) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const call = (key: any, payload?: unknown) => editor.action(callCommand(key, payload));
    switch (command) {
      case "bold":
        call(toggleStrongCommand.key);
        break;
      case "italic":
        call(toggleEmphasisCommand.key);
        break;
      case "code":
        call(toggleInlineCodeCommand.key);
        break;
      case "heading":
        call(wrapInHeadingCommand.key, 2);
        break;
      case "quote":
        call(wrapInBlockquoteCommand.key);
        break;
      case "bullet":
        call(wrapInBulletListCommand.key);
        break;
      case "ordered":
        call(wrapInOrderedListCommand.key);
        break;
      case "codeblock":
        call(createCodeBlockCommand.key);
        break;
      case "table":
        call(insertTableCommand.key, { row: 2, col: 2 });
        break;
      case "task":
        editor.action(insert("- [ ] "));
        break;
      case "link": {
        const url = window.prompt("Link address (https://…)");
        if (!url) break;
        const v = view();
        const { from, to, empty } = v.state.selection;
        const text = empty ? url : v.state.doc.textBetween(from, to);
        const mark = v.state.schema.marks.link?.create({ href: url });
        if (mark) {
          v.dispatch(v.state.tr.replaceWith(from, to, v.state.schema.text(text, [mark])));
        }
        break;
      }
    }
    view().focus();
  };

  return {
    kind: "wysiwyg",
    root: dom,
    popupParent: opts.host,
    anchor: opts.host,
    getMarkdown: () => editor.action(getMarkdown()),
    setMarkdown: (md) => editor.action(replaceAll(md, true)),
    focus: () => view().focus(),
    run,
    insertMarkdown: (md) => {
      editor.action(insert(md, true));
      view().focus();
    },
    textBeforeCaret() {
      const { $from, empty } = view().state.selection;
      if (!empty || !$from.parent.isTextblock) return null;
      return $from.parent.textBetween(0, $from.parentOffset, undefined, "￼");
    },
    replaceBeforeCaret(n, text) {
      const v = view();
      const { from } = v.state.selection;
      const tr = v.state.tr.insertText(text, from - n, from);
      // The caret goes after the inserted text, trailing space included.
      tr.setSelection(TextSelection.create(tr.doc, from - n + text.length));
      v.dispatch(tr);
      v.focus();
    },
    caretRect() {
      const v = view();
      const c = v.coordsAtPos(v.state.selection.from);
      return { left: c.left, bottom: c.bottom };
    },
    onChange(cb) {
      changed = cb;
    },
    destroy() {
      void editor.destroy();
      surface.remove();
    },
  };
}
