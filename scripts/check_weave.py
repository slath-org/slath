#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Weave rules for templates (part of scripts/guards.sh).

Template rules (crates/http/templates, every file but the catalog's):
  details-indicator  every <details> is a disclosure, menu, popover, or
                     reply box, or its summary ends with the turning chevron
                     (span.disclosure); never the browser's bare marker
  heading-skip       no heading more than one level below the one before it
                     in the same template
  field-group        a fieldset holding two or more fields is a group of
                     fields (class field-group)
  inline-style       no style attributes (spacing comes from components)
  empty-state        an empty state is the empty state component (msg::empty
                     or p.empty-state) and a sentence with its full stop,
                     never a bare secondary paragraph ("None", "No one")
  status-markup      a status is the status line component (m::status_line,
                     m::status_text); a tone's icon written out by hand only
                     in its list form (li.tone-*), a requirement's summary,
                     the readiness card's icon, a list row's readiness, and
                     a list group's heading

Catalog rules:
  catalog-class      every class a page template uses is shown in the design
                     catalog (crates/http/templates/design, the component
                     macros, and what they include)
  catalog-unused     every catalog entry is used by some page

Known violations, the pages not yet moved to the components, are listed in
scripts/weave-allowlist.txt as `<rule> <key>  # reason` (the key is the
template path for template rules, the class for catalog-class, the entry id
for catalog-unused). The list only shrinks: an entry that no longer
matches a violation is an error too.

Usage: check_weave.py ROOT   Exit status 1 on violations.
"""
import re
import sys
from pathlib import Path

TEMPLATES = Path("crates/http/templates")
ALLOWLIST = Path("scripts/weave-allowlist.txt")
EXPR = re.compile(r"{%.*?%}|{{.*?}}|{#.*?#}", re.S)

# Where each catalog entry shows up in page templates: markup or macro calls.
# "*" means the entry is always used (the frame draws it, or scripts do).
ENTRY_MARKERS = {
    "type": "*", "code": r"<code", "buttons": r'class="button', "icons": r"m::icon\(",
    "global-bar": "*", "side-menu": r"m::side_menu\(", "context-header": r'class="repo-header|fr::context_header\(',
    "stack": r'class="(?:[^"]* )?stack(?:-inside|-groups|-sections)?[ "]', "cluster": r'class="(?:[^"]* )?cluster[ "]',
    "page": r'class="(?:[^"]* )?page-head[ "]', "section": r'class="(?:[^"]* )?section[ "]',
    "subsection": r'class="(?:[^"]* )?subsection[ "]', "frame": r'class="(?:[^"]* )?frame[ "]', "detail": r'class="detail"',
    "notice": r"msg::notice\(|class=\"notice", "saved": r"msg::saved\(|success-summary",
    "error-summary": r"msg::error_summary\(|error-summary", "notice-line": r"msg::notice_line\(|notice-line",
    "empty": r"msg::empty\(|empty-state", "busy": "*", "toast": "*",
    "form": r"form-layout", "field": r"f::field\(|class=\"field", "question": r"f::question\(|class=\"question", "option-follow-up": r"f::option_follow_up\(",
    "field-group": r"f::field_group\(|field-group", "form-buttons": r"f::buttons\(|form-buttons",
    "copy-value": r"f::copy_value\(|m::copy_field\(", "danger": r"f::danger\(",
    "disclosure": r"d::disclosure\(|details class=\"disclosure", "menu": r'class="menu[ "]|d::menu\(',
    "reply-box": r"d::reply_box\(", "choice": r'class="choice"',
    "rows": r"item-list", "groups": r"item-group-heading", "table": r"table-records", "facts": r'class="facts"', "pager": r"l::pager\(",
    "more": r"l::more\(",
    "account": r"p::account\(", "avatar-upload": r'class="profile-avatar"', "git-identity": r"p::git_identity\(", "avatar-group": r"p::avatar_group\(",
    "links": r"c::external\(", "tooltip": r"data-tip=", "time": r"c::time\(", "hash": r"c::hash\(",
    "cut": r"c::cut\(", "popover": r"c::popover\(", "password": r'type="password"', "count": r"maxlength=",
    "names": r"data-names", "name-field": r"data-suggest-single", "sort": r"m::view_menu\(ctx, \"Sort\"", "switches": r'type="checkbox"', "dialog": "*", "shortcuts": "*", "palette": "*",
    "status-line": r"m::tone_icon\(|m::status_line\(|m::status_text\(", "readiness": r"readiness_card", "timeline": r'class="timeline"',
    "thread": r"timeline-thread", "chips": r'class="badge',
    "code-toolbar": r"repo/ref_line\.html", "file-list": r"table file-list", "code-view": r"code-table",
    "readme": r"section readme", "commit-message": r"commit-body",
    "diff": r"diff_files\.html", "markdown": r'class="markdown', "editor": r"<slath-editor",
}


def strip(text: str) -> str:
    """The template with its expressions and comments blanked out."""
    return EXPR.sub(lambda m: " " * len(m.group(0)) if "\n" not in m.group(0) else "\n" * m.group(0).count("\n"), text)


def line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def check_template(path: str, text: str) -> list[tuple[str, str]]:
    """Template rule violations as (rule, message)."""
    out = []
    plain = strip(text)
    for m in re.finditer(r"<details\b([^>]*)>(.*?)</summary>", text, re.S):
        attrs, head = m.group(1), m.group(2)
        cls = re.search(r'class="([^"]*)"', attrs)
        kinds = set(cls.group(1).split()) if cls else set()
        if kinds & {"disclosure", "menu", "reply-box", "popover"} or 'class="disclosure"' in head:
            continue
        out.append(("details-indicator", f"{path}:{line_of(text, m.start())}: <details> without the disclosure or menu component"))
    prev = None
    for m in re.finditer(r"<h([1-6])\b", plain):
        level = int(m.group(1))
        if prev is not None and level > prev + 1:
            out.append(("heading-skip", f"{path}:{line_of(text, m.start())}: h{level} after h{prev}"))
        prev = level
    for m in re.finditer(r"<fieldset\b([^>]*)>(.*?)</fieldset>", text, re.S):
        cls = re.search(r'class="([^"]*)"', m.group(1))
        if cls and "field-group" in cls.group(1).split():
            continue
        inner = m.group(2)
        fields = len(re.findall(r'class="field[ "]', inner)) + len(re.findall(r"f::field\(", inner))
        if fields >= 2:
            out.append(("field-group", f"{path}:{line_of(text, m.start())}: a fieldset with {fields} fields isn't a group of fields"))
    for m in re.finditer(r"\sstyle=", plain):
        out.append(("inline-style", f"{path}:{line_of(text, m.start())}: style attribute"))
    for m in re.finditer(r'msg::empty\("([^"]*)"\)', text):
        if not m.group(1).rstrip().endswith((".", "?", "!")):
            out.append(("empty-state", f"{path}:{line_of(text, m.start())}: empty state without its full stop"))
    for m in re.finditer(r'<p class="empty-state">(.*?)</p>', text, re.S):
        words = re.sub(r"<[^>]+>", "", EXPR.sub("", m.group(1))).rstrip()
        if words and not words.endswith((".", "?", "!")):
            out.append(("empty-state", f"{path}:{line_of(text, m.start())}: empty state without its full stop"))
    for m in re.finditer(r'<p class="text-secondary">\s*(No |None\b|Nothing\b)', plain):
        out.append(("empty-state", f"{path}:{line_of(text, m.start())}: empty state as a secondary paragraph; use msg::empty"))
    for m in re.finditer(r"m::tone_icon\(", text):
        before = EXPR.sub("", text[: m.start()])
        tags = re.findall(r"<[a-z][^>]*>", before)
        last = tags[-1] if tags else ""
        if re.match(r'<li\b[^>]*class="[^"]*tone-|<summary>|<span class="(?:readiness-icon|item-checks|item-group-tone)', last):
            continue
        out.append(("status-markup", f"{path}:{line_of(text, m.start())}: a status written out; use m::status_line or m::status_text"))
    return out


def classes(text: str) -> set[str]:
    out = set()
    for m in re.finditer(r'\bclass="([^"]*)"', text):
        value = re.sub(r"{{.*?}}", "{}", re.sub(r"{%.*?%}", " ", m.group(1)))
        for c in value.split():
            out.add(c)
    return out


def catalog_files(root: Path) -> list[Path]:
    """The catalog's templates, the component macros, and what they include."""
    base = root / TEMPLATES
    # Every catalog page is drawn in the frame (base.html), so the global
    # bar is shown there too.
    todo = sorted((base / "design").glob("*.html")) + sorted((base / "components").glob("*.html")) + [base / "macros.html", base / "base.html"]
    seen = []
    while todo:
        p = todo.pop()
        if p in seen or not p.exists():
            continue
        seen.append(p)
        for inc in re.findall(r'{%-?\s*include\s+"([^"]+)"', p.read_text(encoding="utf-8")):
            todo.append(base / inc)
    return seen


def page_files(root: Path, catalog: list[Path]) -> list[Path]:
    base = root / TEMPLATES
    design = base / "design"
    return [p for p in sorted(base.rglob("*.html")) if design not in p.parents and p not in catalog and p.name != "design.html"]


def matches(cls: str, shown: set[str]) -> bool:
    """A used class (maybe with a {} placeholder) is shown in the catalog."""
    if cls.endswith("-"):
        cls += "{}"
    if "{}" not in cls:
        return cls in shown
    rx = re.compile("^" + re.escape(cls).replace(r"\{\}", ".+") + "$")
    return any(rx.match(s) for s in shown)


def check_catalog(root: Path) -> list[tuple[str, str, str]]:
    """Catalog violations as (rule, key, message)."""
    cat = catalog_files(root)
    shown = set()
    for p in cat:
        shown |= classes(p.read_text(encoding="utf-8"))
    pages = page_files(root, cat)
    out = []
    used: dict[str, str] = {}
    for p in pages:
        for c in classes(p.read_text(encoding="utf-8")):
            used.setdefault(c, str(p.relative_to(root)))
    for c, where in sorted(used.items()):
        if not matches(c, shown):
            out.append(("catalog-class", c, f"class {c} (first in {where}) has no catalog entry"))
    texts = "\n".join(p.read_text(encoding="utf-8") for p in pages)
    entries = []
    for p in sorted((root / TEMPLATES / "design").glob("*.html")):
        entries += re.findall(r'cat::entry\("([^"]+)"', p.read_text(encoding="utf-8"))
    for e in entries:
        marker = ENTRY_MARKERS.get(e)
        if marker is None:
            out.append(("catalog-unused", e, f"catalog entry {e} has no marker in check_weave.py"))
        elif marker != "*" and not re.search(marker, texts):
            out.append(("catalog-unused", e, f"catalog entry {e} isn't used by any page"))
    return out


def load_allowlist(root: Path) -> set[tuple[str, str]]:
    p = root / ALLOWLIST
    if not p.exists():
        return set()
    out = set()
    for line in p.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        rule, key, *_ = line.split()
        out.add((rule, key))
    return out


def run(root: Path) -> list[str]:
    allow = load_allowlist(root)
    found: set[tuple[str, str]] = set()
    errors = []
    base = root / TEMPLATES
    design = base / "design"
    for p in sorted(base.rglob("*.html")):
        if design in p.parents or p.name == "design.html":
            continue
        rel = str(p.relative_to(root))
        for rule, msg in check_template(rel, p.read_text(encoding="utf-8")):
            found.add((rule, rel))
            if (rule, rel) not in allow:
                errors.append(f"weave {rule}: {msg}")
    for rule, key, msg in check_catalog(root):
        found.add((rule, key))
        if (rule, key) not in allow:
            errors.append(f"weave {rule}: {msg}")
    for rule, key in sorted(allow - found):
        errors.append(f"weave allowlist: `{rule} {key}` no longer applies; remove it from {ALLOWLIST}")
    return errors


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    errors = run(root)
    for e in errors:
        print(e, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
