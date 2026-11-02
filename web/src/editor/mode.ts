// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// What the editor shell needs from an editing mode (rich text or source).

export type Command =
  | "bold"
  | "italic"
  | "code"
  | "link"
  | "heading"
  | "quote"
  | "bullet"
  | "ordered"
  | "task"
  | "codeblock"
  | "table";

export interface Mode {
  readonly kind: "wysiwyg" | "source";
  /** The focusable editing element. */
  readonly root: HTMLElement;
  /** Where popups go so IDREFs reach `root` (a shadow root for source mode). */
  readonly popupParent: HTMLElement | ShadowRoot;
  /** The element popups are positioned against. */
  readonly anchor: HTMLElement;
  getMarkdown(): string;
  setMarkdown(markdown: string): void;
  focus(): void;
  run(command: Command): void;
  /** Inserts Markdown at the caret (attachments, references). */
  insertMarkdown(markdown: string): void;
  /** Text of the current block before the caret, or null. */
  textBeforeCaret(): string | null;
  /** Replaces the `n` characters before the caret with `text`. */
  replaceBeforeCaret(n: number, text: string): void;
  /** Viewport coordinates below the caret. */
  caretRect(): { left: number; bottom: number } | null;
  /** Called after every change the person makes. */
  onChange(cb: () => void): void;
  destroy(): void;
}

export interface MountOptions {
  host: HTMLElement;
  markdown: string;
  /** Accessible name of the editing element. */
  label: string;
  /** Images from other sites may load (the instance allows them). */
  externalImages?: boolean;
}
