# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_templates.py."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_templates import check_script, check_template  # noqa: E402

P = Path("t.html")
STAMP = "{% call m::hx(ctx.nonce) %}{% endcall %}"


class TemplateRules(unittest.TestCase):
    def ok(self, text):
        self.assertEqual(check_template(P, text), [], text)

    def bad(self, text, fragment):
        errs = check_template(P, text)
        self.assertTrue(any(fragment in e for e in errs), (text, errs))

    def test_stamped_elements_pass(self):
        self.ok(f'<a href="/x" hx-get="/x" hx-target="#r" {STAMP}>x</a>')
        self.ok(f'<a href="/x"\n   hx-get="/x"\n   {STAMP}>x</a>')
        self.ok('<div hx-ignore>{{ user_html }}</div>')
        self.ok('<p>Use hx-get in prose</p>')
        self.ok("<meta name=\"htmx-config\" content='extensions:\"hx-csp\"'>")

    def test_unstamped_hx_get_fails(self):
        self.bad('<a href="/x" hx-get="/x">x</a>', "lacks")
        self.bad('<a data-hx-post="/x">x</a>', "lacks")
        self.bad('<a hx-get="/x" hx-nonce="{{ ctx.nonce }}">x</a>', "macro")

    def test_forbidden_features(self):
        self.bad(f'<button hx-on:click="alert(1)" {STAMP}>x</button>', "hx-on")
        self.bad(f'<button hx-on::before:request="x" {STAMP}>x</button>', "hx-on")
        self.bad(f'<div hx-live="x" {STAMP}></div>', "hx-live")
        self.bad(f'<a hx-get="/x" hx-vals="js:{{a:1}}" {STAMP}>x</a>', "js:")
        self.bad(f'<a hx-get="/x" hx-confirm="javascript:x()" {STAMP}>x</a>', "js:")
        self.bad(f'<a hx-get="/x" hx-trigger="click[ctrlKey]" {STAMP}>x</a>', "filters")
        self.bad("<meta name=\"htmx-config\" content='prefix:\"data-x-\"'>", "prefix")
        self.bad("<meta name=\"htmx-config\" content='safeEval:true'>", "safeEval")


class ScriptRules(unittest.TestCase):
    def test_scripts(self):
        self.assertEqual(check_script(P, "htmx.trigger(el, 'x');"), [])
        for bad in ("el.setAttribute('hx-on:click', 'x')", "htmx.config.safeEval = true",
                    "htmx.config.prefix = 'x'", "eval('1')", "new Function('x')"):
            self.assertTrue(check_script(P, bad), bad)


if __name__ == "__main__":
    unittest.main()
