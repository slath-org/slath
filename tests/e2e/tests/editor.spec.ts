// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The Markdown editor (EDITOR-01): corpus fidelity, keyboard, mouse, and
// touch flows in both themes, reference suggestions, source mode, preview,
// attachments, drafts, the no-JavaScript fallback, and loading on demand.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { USERS, editor, expect, expectAccessible, richText, signIn, tapOrClick, test } from "../fixtures";

const CORPUS = resolve(import.meta.dirname, "../../../web/tests/markdown-roundtrip");
const manifest: { files: Record<string, { lossless: boolean }> } = JSON.parse(
  readFileSync(resolve(CORPUS, "manifest.json"), "utf8"),
);

async function newIssuePage(page: import("@playwright/test").Page) {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  await expect(page.locator("slath-editor[data-state=ready]")).toBeVisible();
}

async function roundtrip(page: import("@playwright/test").Page, md: string): Promise<string> {
  return page.evaluate(async (text) => {
    const el = document.querySelector("slath-editor") as HTMLElement & {
      slathEditor: { roundtrip(md: string): Promise<string> };
    };
    return el.slathEditor.roundtrip(text);
  }, md);
}

test("conf_editor_01_corpus_round_trip_and_idempotency", async ({ page }, testInfo) => {
  test.skip(!testInfo.project.name.endsWith("light-desktop"), "the corpus runs once per browser");
  test.slow();
  await newIssuePage(page);
  const rewritten: string[] = [];
  for (const [name, meta] of Object.entries(manifest.files)) {
    const md = readFileSync(resolve(CORPUS, name), "utf8");
    const once = await roundtrip(page, md);
    expect(await roundtrip(page, once), `${name} is idempotent`).toBe(once);
    if (meta.lossless && once.trimEnd() !== md.trimEnd()) rewritten.push(name);
  }
  // Lossless documents the rich editor would still restyle must open in
  // source mode (checked below), so saving never changes them.
  for (const name of rewritten) {
    const md = readFileSync(resolve(CORPUS, name), "utf8");
    await page.goto("/alice/demo/issues/new");
    await page.evaluate((text) => {
      localStorage.clear();
      const ta = document.querySelector("slath-editor textarea") as HTMLTextAreaElement;
      ta.value = text;
    }, md);
    // A fresh editor on that text.
    await page.evaluate(async (text) => {
      const host = document.createElement("slath-editor");
      host.dataset.repo = "alice/demo";
      const ta = document.createElement("textarea");
      ta.id = "probe";
      ta.value = text;
      host.append(ta);
      document.querySelector("main")!.append(host);
      const shell = await import(document.querySelector<HTMLScriptElement>("script[type=module]")!.src.replace("app.js", "shell.js"));
      await shell.mount(host);
    }, md);
    const mode = await page.evaluate(() => {
      const hosts = document.querySelectorAll("slath-editor");
      const el = hosts[hosts.length - 1] as HTMLElement & { slathEditor: { mode(): string; markdown(): string } };
      return [el.slathEditor.mode(), el.slathEditor.markdown()];
    });
    expect(mode[0], `${name} opens in source mode`).toBe("source");
    expect(mode[1], `${name} is unchanged`).toBe(md);
  }
});

test("conf_editor_01_saving_without_edits_keeps_the_text", async ({ page }) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  const md = readFileSync(resolve(CORPUS, "23-footnotes.md"), "utf8");
  await page.getByLabel("Title").fill("Untouched body");
  // Text typed without the editor (as a draft restored from another tab would be).
  await page.evaluate((text) => {
    (document.querySelector("slath-editor textarea") as HTMLTextAreaElement).value = text;
  }, md);
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Untouched body");
  await tapOrClick(page, page.getByRole("link", { name: "Edit issue", exact: true }));
  // The issue page's comment editor is ready too: wait for the edit page.
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Edit issue");
  await expect(page.locator("slath-editor[data-state=ready]")).toBeVisible();
  // Saving without touching the text sends it back unchanged.
  await tapOrClick(page, page.getByRole("button", { name: "Save" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Untouched body");
  await tapOrClick(page, page.getByRole("link", { name: "Edit issue", exact: true }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Edit issue");
  expect(await page.locator("slath-editor textarea").inputValue()).toBe(md);
});

test("conf_editor_01_keyboard_flow_with_suggestions", async ({ page }, testInfo) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  await richText(page);
  const title = `Editor ${testInfo.project.name}`;
  await page.getByLabel("Title").fill(title);
  const box = editor(page, "Description");
  await box.click();
  await page.keyboard.type("Hello ");
  await page.keyboard.press("ControlOrMeta+b");
  await page.keyboard.type("world");
  await page.keyboard.press("ControlOrMeta+b");
  await page.keyboard.type(" cc @ali");
  const list = page.getByRole("listbox", { name: "Suggestions" });
  await expect(list).toBeVisible();
  await expect(box).toHaveAttribute("aria-activedescendant", /editor-suggestions-/);
  await expectAccessible(page);
  await page.keyboard.press("Enter");
  await expect(list).toBeHidden();
  await page.keyboard.type("and ~");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Backspace");
  // Tab leaves the editor.
  await page.keyboard.press("Tab");
  await expect(box).not.toBeFocused();
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);
  const body = page.locator(".issue-body, .markdown").first();
  await expect(body.locator("strong")).toHaveText("world");
  await expect(body.getByRole("link", { name: "@alice" })).toBeVisible();
});

test("conf_editor_01_toolbar_source_mode_preview_and_drafts", async ({ page }) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  await richText(page);
  const toolbar = page.getByRole("toolbar", { name: "Formatting" });
  await expect(toolbar).toBeVisible();
  const box = editor(page, "Description");
  await tapOrClick(page, box);
  await page.keyboard.type("Item");
  await tapOrClick(page, toolbar.getByRole("button", { name: "Bulleted list" }));
  await expectAccessible(page);
  // Source mode shows the Markdown.
  await tapOrClick(page, page.getByRole("button", { name: "Switch to Markdown source" }));
  const source = editor(page, "Description");
  await expect(source).toContainText("- Item");
  await tapOrClick(page, source);
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("\n\n**Draft text**");
  await expectAccessible(page);
  // Preview is the server's rendering.
  await tapOrClick(page, page.getByRole("tab", { name: "Preview" }));
  const preview = page.getByRole("tabpanel", { name: "Preview" });
  await expect(preview.locator("strong")).toHaveText("Draft text");
  await expect(preview.locator("li")).toHaveText("Item");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("tab", { name: "Write" }));
  // The draft survives a reload until the form is sent.
  await page.waitForTimeout(600);
  await page.reload();
  await expect(page.getByText("Restored text you didn't send yet.")).toBeVisible();
  await expect(editor(page, "Description")).toContainText("Draft text");
  // The restored text has Markdown the rich editor would restyle, so it
  // stays in source mode; discarding it brings back the empty field.
  await tapOrClick(page, page.getByRole("button", { name: "Discard it" }));
  await expect(page.getByText("Restored text you didn't send yet.")).toBeHidden();
  await richText(page);
});

test("conf_editor_01_attachment_upload_by_button", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  await richText(page);
  await tapOrClick(page, editor(page, "Description"));
  await expect(page.getByRole("toolbar", { name: "Formatting" }).getByRole("button", { name: "Attach files" })).toBeVisible();
  // The button opens this file picker.
  await page.locator("slath-editor input[type=file]").setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello"),
  });
  await expect(page.getByRole("status").filter({ hasText: "Attached notes.txt" })).toBeVisible();
  await expect(editor(page, "Description").getByRole("link", { name: "notes.txt" })).toBeVisible();
});

test("conf_editor_01_without_javascript", async ({ browser, baseURL }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, baseURL });
  const page = await context.newPage();
  await page.goto("/-/login");
  await page.getByLabel("Username").fill(USERS.alice.name);
  await page.getByLabel("Password").fill(USERS.alice.password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  // Wait for the sign-in to finish; navigating earlier cancels it.
  await expect(page).not.toHaveURL(/\/-\/login/);
  await page.goto("/alice/demo/issues/new");
  await page.getByLabel("Title").fill("No script");
  await page.getByLabel("Description").fill("**plain** textarea");
  await page.getByRole("button", { name: "Open issue" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("No script");
  await expect(page.locator(".markdown strong").first()).toHaveText("plain");
  await context.close();
});

test("conf_editor_01_loaded_only_with_an_editor", async ({ page }) => {
  const scripts: string[] = [];
  page.on("request", (r) => {
    if (r.resourceType() === "script") scripts.push(new URL(r.url()).pathname);
  });
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await page.waitForLoadState("networkidle");
  expect(scripts.filter((s) => /(shell|wysiwyg|source)\.js$/.test(s))).toEqual([]);
  await page.goto("/alice/demo/issues/new");
  await expect(page.locator("slath-editor[data-state=ready]")).toBeVisible();
  expect(scripts.some((s) => s.endsWith("/shell.js"))).toBe(true);
  // Exactly one editing mode was loaded.
  expect(scripts.filter((s) => /\/(wysiwyg|source)\.js$/.test(s)).length).toBe(1);
});
