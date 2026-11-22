// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's forms family: options read as options, legends line up
// with labels, groups of fields stand apart, buttons keep their width.
import { expect, expectAccessible, test } from "../fixtures";

test("conf_design_03_forms_family_accessible", async ({ page }) => {
  await page.goto("/-/design/forms");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Forms");
  await expectAccessible(page);
});

test("conf_design_03_options_regular_legends_aligned", async ({ page }) => {
  await page.goto("/-/design/forms");
  const q = page.locator("#demo-visibility");
  const m = await q.evaluate((el) => {
    const legend = el.querySelector("legend")!;
    const option = el.querySelector(".option label")!;
    const field = document.querySelector("label[for=demo-desc]")!;
    return {
      legendWeight: getComputedStyle(legend).fontWeight,
      optionWeight: getComputedStyle(option).fontWeight,
      legendX: Math.round(legend.getBoundingClientRect().left),
      labelX: Math.round(field.getBoundingClientRect().left),
    };
  });
  expect(m.legendWeight).toBe("600");
  expect(m.optionWeight).toBe("400");
  expect(m.legendX).toBe(m.labelX);
});

test("conf_design_03_field_group_stands_apart", async ({ page }) => {
  await page.goto("/-/design/forms");
  const gaps = await page.locator("section[aria-labelledby=field-group] .form-layout").evaluate((el) => {
    const k = [...el.children].map((c) => c.getBoundingClientRect());
    return k.slice(1).map((r, i) => Math.round(r.top - k[i].bottom));
  });
  // Field -> group, group -> buttons: the groups distance, not the items one.
  expect(gaps).toEqual([24, 24]);
  const inner = await page.locator("section[aria-labelledby=field-group] .field-group").evaluate((el) => {
    const f = [...el.querySelectorAll(":scope > .field")].map((c) => c.getBoundingClientRect());
    return Math.round(f[1].top - f[0].bottom);
  });
  expect(inner).toBe(16);
});

test("conf_design_03_buttons_keep_their_width_and_errors_link", async ({ page }) => {
  await page.goto("/-/design/forms");
  const widths = await page.locator("section[aria-labelledby=form] .form-layout").first().evaluate((form) => ({
    form: form.getBoundingClientRect().width,
    button: form.querySelector(".form-buttons .button")!.getBoundingClientRect().width,
  }));
  expect(widths.button).toBeLessThan(widths.form / 2);
  const input = page.locator("#demo-name-2");
  await expect(input).toHaveAttribute("aria-invalid", "true");
  await expect(input).toHaveAttribute("aria-describedby", "demo-name-2-hint demo-name-2-error");
});

test("conf_design_03_option_follow_up_lines_up_with_its_option", async ({ page }) => {
  await page.goto("/-/design/forms");
  const field = page.locator("#demo-defer");
  await expect(field).toBeHidden();
  await page.locator("#demo-verdict-3").check();
  await expect(field).toBeVisible();
  // No line at its start: the follow-up lines up with its option's name.
  const m = await page.locator("#demo-verdict-3-follow-up").evaluate((el) => {
    const label = document.querySelector("label[for=demo-verdict-3]")!;
    return {
      border: getComputedStyle(el).borderInlineStartWidth,
      left: Math.round(el.getBoundingClientRect().left),
      labelLeft: Math.round(label.getBoundingClientRect().left),
      fieldLeft: Math.round(document.querySelector("#demo-defer")!.getBoundingClientRect().left),
    };
  });
  expect(m.border).toBe("0px");
  expect(m.left).toBe(m.labelLeft);
  expect(m.fieldLeft).toBe(m.labelLeft);
  await expectAccessible(page);
});
