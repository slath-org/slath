-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Content trust per work order: whose text and merge requests its task
-- token reads. `members` (the default) admits text from people with a
-- role or organization membership and from bots with a role; `writers`
-- only from those with the write role; `all` withholds nothing.

ALTER TABLE work_orders
    ADD COLUMN content_trust text NOT NULL DEFAULT 'members'
        CHECK (content_trust IN ('all', 'members', 'writers'));

UPDATE schema_info SET version = 162;
