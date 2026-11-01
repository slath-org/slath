#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Writes crates/core/tests/matrix.csv: the authorization matrix, cell by cell.

This is a transcription of the documented permission table, written
independently of the policy code. crates/core/tests/matrix.rs checks the
policy against the CSV, so a policy change that isn't reflected here (and
the other way round) fails the tests. Regenerate after changing the table:

    python3 scripts/gen_authz_matrix.py
"""
import sys
from pathlib import Path

# action: (anonymous on a public repo, read, write, maintain, admin)
Y, N = True, False
TABLE = {
    "repo.read": (Y, Y, Y, Y, Y),
    "repo.fork": (N, Y, Y, Y, Y),
    "repo.push": (N, N, Y, Y, Y),
    # Protected branches refine this per rule (push_role) on top of the policy.
    "repo.push_protected": (N, N, Y, Y, Y),
    "repo.push_maintainer_edit": (N, N, Y, Y, Y),
    "tag.move_or_delete": (N, N, N, Y, Y),
    "issue.create": (N, Y, Y, Y, Y),
    "comment.create": (N, Y, Y, Y, Y),
    "issue.edit_own": (N, Y, Y, Y, Y),
    "comment.edit_own": (N, Y, Y, Y, Y),
    "issue.triage": (N, N, Y, Y, Y),
    "comment.moderate": (N, N, N, Y, Y),
    "mr.create": (N, N, Y, Y, Y),
    "mr.comment": (N, Y, Y, Y, Y),
    "mr.review": (N, Y, Y, Y, Y),
    "mr.suggest": (N, Y, Y, Y, Y),
    "mr.edit_own": (N, Y, Y, Y, Y),
    "mr.triage": (N, N, Y, Y, Y),
    "mr.approve": (N, N, Y, Y, Y),
    "mr.merge": (N, N, Y, Y, Y),
    "mr.auto_merge": (N, N, Y, Y, Y),
    # Writes to the source: push permission there (checked on the source repo).
    "mr.update_branch": (N, N, Y, Y, Y),
    "mr.apply_suggestion": (N, N, Y, Y, Y),
    "status.write": (N, N, Y, Y, Y),
    "ci.approve_external_run": (N, N, Y, Y, Y),
    "ci.rerun": (N, N, Y, Y, Y),
    "ci.cancel": (N, N, Y, Y, Y),
    "labels.manage": (N, N, N, Y, Y),
    "milestones.manage": (N, N, N, Y, Y),
    "protection.manage": (N, N, N, N, Y),
    "webhooks.manage": (N, N, N, N, Y),
    "secrets.manage": (N, N, N, N, Y),
    "collaborators.manage": (N, N, N, N, Y),
    "repo.settings": (N, N, N, N, Y),
    "repo.rename": (N, N, N, N, Y),
    "repo.archive": (N, N, N, N, Y),
    "repo.delete": (N, N, N, N, Y),
    "repo.transfer": (N, N, N, N, Y),
    "repo.visibility": (N, N, N, N, Y),
}

# Token scope that covers each action group.
SCOPE = {a: "repo:write" for a in TABLE}
SCOPE["repo.read"] = "repo:read"
for a in ("issue.create", "comment.create", "issue.edit_own", "comment.edit_own", "issue.triage",
          "comment.moderate", "labels.manage", "milestones.manage"):
    SCOPE[a] = "issues:write"
for a in TABLE:
    if a.startswith("mr."):
        SCOPE[a] = "mrs:write"
SCOPE["status.write"] = "statuses:write"
SCOPES = ["repo:read", "repo:write", "issues:write", "mrs:write", "statuses:write", "user:read", "user:write", "admin"]
IMPLIES = {("repo:write", "repo:read"), ("user:write", "user:read")}

# Actions still allowed on archived repositories.
READS = {"repo.read", "repo.fork"}
ROLES = ["read", "write", "maintain", "admin"]


def implies(have: str, need: str) -> bool:
    return have == need or (have, need) in IMPLIES


def rows():
    for action, cells in TABLE.items():
        anon, *by_role = cells
        for vis in ("public", "private"):
            for state in ("active", "archived"):
                def decide(allowed: bool, can_read: bool) -> str:
                    if not can_read:
                        return "deny_hidden"
                    if state == "archived" and action not in READS and action != "repo.archive":
                        return "deny_visible"
                    return "allow" if allowed else "deny_visible"

                yield (action, vis, state, "anon", "none", "session",
                       decide(anon, vis == "public"))
                # A signed-in user without a role reads public repos implicitly.
                yield (action, vis, state, "user", "none", "session",
                       decide(by_role[0], vis == "public"))
                for role, allowed in zip(ROLES, by_role):
                    yield (action, vis, state, "user", role, "session", decide(allowed, True))
        # Suspended users and repositories being deleted: nothing, invisibly.
        yield (action, "public", "active", "suspended", "admin", "session", "deny_hidden")
        yield (action, "private", "deleting", "user", "admin", "session", "deny_hidden")
        yield (action, "public", "deleting", "anon", "none", "session", "deny_hidden")
        # Tokens: the scope caps an admin's role.
        for scope in SCOPES:
            ok = implies(scope, SCOPE[action])
            yield (action, "private", "active", "user", "admin", f"token:{scope}", "allow" if ok else "deny_visible")
        # A token restricted to other repositories acts as an anonymous visitor.
        yield (action, "private", "active", "user", "admin", "token-other:repo:write", "deny_hidden")
        yield (action, "public", "active", "user", "admin", "token-other:repo:write",
               "allow" if anon else "deny_visible")
        # Forking needs the repository to allow it.
        if action == "repo.fork":
            yield (action, "public", "active", "user", "admin", "session-noforks", "deny_visible")


def render() -> str:
    lines = [
        "# Generated by scripts/gen_authz_matrix.py from the documented permission table.",
        "# subject: anon | user (active human) | suspended",
        "# role: the subject's effective role (none | read | write | maintain | admin)",
        "# credential: session | session-noforks (repo disallows forks) | token:<scope> | token-other:<scope> (restricted to another repo)",
        "# expect: allow | deny_hidden (404) | deny_visible (403)",
        "action,visibility,state,subject,role,credential,expect",
    ]
    lines += [",".join(r) for r in rows()]
    return "\n".join(lines) + "\n"


OUT = Path(__file__).resolve().parent.parent / "crates/core/tests/matrix.csv"


def main(argv: list[str]) -> int:
    text = render()
    if "--check" in argv:
        if OUT.read_text(encoding="utf-8") != text:
            print("authz-matrix: matrix.csv differs from the table; run scripts/gen_authz_matrix.py", file=sys.stderr)
            return 1
        return 0
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {text.count(chr(10)) - 6} rows to {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
