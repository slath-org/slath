# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_weave.py."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_weave import check_template, matches, run  # noqa: E402


def rules(text):
    return [r for r, _ in check_template("t.html", text)]


class TemplateRules(unittest.TestCase):
    def test_disclosures_need_the_component(self):
        self.assertEqual(rules("<details><summary>More</summary></details>"), ["details-indicator"])
        self.assertEqual(rules('<details class="issue-filters"><summary>Filter</summary></details>'), ["details-indicator"])
        for ok in (
            '<details class="disclosure"><summary>More</summary></details>',
            '<details class="menu view-menu"><summary class="button">State</summary></details>',
            '<details class="reply-box"><summary>Reply…</summary></details>',
            '<details><summary>Rows<span class="disclosure">v</span></summary></details>',
        ):
            self.assertEqual(rules(ok), [], ok)

    def test_heading_levels_never_skip(self):
        self.assertEqual(rules("<h1>a</h1><h3>b</h3>"), ["heading-skip"])
        self.assertEqual(rules("<h1>a</h1><h2>b</h2><h3>c</h3><h2>d</h2>"), [])
        # Headings inside template expressions don't count.
        self.assertEqual(rules('<h1>a</h1>{{ "<h4>" }}<h2>b</h2>'), [])

    def test_two_fields_make_a_group(self):
        two = '<div class="field"></div><div class="field"></div>'
        self.assertEqual(rules(f"<fieldset><legend>x</legend>{two}</fieldset>"), ["field-group"])
        self.assertEqual(rules(f'<fieldset class="field-group"><legend>x</legend>{two}</fieldset>'), [])
        self.assertEqual(rules('<fieldset class="question"><legend>x</legend><div class="option"></div></fieldset>'), [])

    def test_no_inline_style(self):
        self.assertEqual(rules('<p style="margin:0">x</p>'), ["inline-style"])
        self.assertEqual(rules("<p>style=fine in text</p>"), [])


    def test_statuses_use_the_component(self):
        icon = '{% call m::tone_icon(ctx, "done") %}{% endcall %}'
        for bad in (f'<span class="tone-done">{icon}<span>ok</span></span>', f'<p class="tone-done">{icon}<span>ok</span></p>'):
            self.assertEqual(rules(bad), ["status-markup"], bad)
        for ok in (
            '{% call m::status_text(ctx, "done", "ok") %}{% endcall %}',
            f'<li class="tone-{{{{ t }}}}">{icon}<span>ok</span></li>',
            f'<li class="requirement tone-done"><details><summary>{icon}<span>ok<span class="disclosure">v</span></span></summary></details></li>',
            f'<span class="readiness-icon">{icon}</span>',
            f'<span class="item-checks tone-done" data-tip="Ready">{icon}<span class="visually-hidden">Ready</span></span>',
        ):
            self.assertEqual(rules(ok), [], ok)


class Catalog(unittest.TestCase):
    def test_placeholders_match_as_prefixes(self):
        shown = {"item-state-open", "tone-done"}
        self.assertTrue(matches("item-state-{}", shown))
        self.assertTrue(matches("tone-", shown))
        self.assertFalse(matches("state-{}", shown))
        self.assertFalse(matches("other", shown))

    def test_allowlist_only_shrinks(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            t = root / "crates/http/templates"
            (t / "design").mkdir(parents=True)
            (root / "scripts").mkdir()
            (t / "design/d.html").write_text('<p class="shown">x</p>')
            (t / "page.html").write_text('<p class="shown">x</p><details><summary>x</summary></details>')
            self.assertEqual(len(run(root)), 1)
            (root / "scripts/weave-allowlist.txt").write_text("details-indicator crates/http/templates/page.html  # not yet\n")
            self.assertEqual(run(root), [])
            (t / "page.html").write_text('<p class="shown">x</p>')
            errors = run(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("no longer applies", errors[0])


if __name__ == "__main__":
    unittest.main()
