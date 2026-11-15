-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A pending reply can carry its thread's resolution: when its review is
-- submitted, the thread is resolved by the reviewer. A reply sent at once
-- resolves its thread at once, so only pending replies keep the flag.

ALTER TABLE mr_thread_comments
    ADD COLUMN resolves bool NOT NULL DEFAULT false,
    ADD CONSTRAINT mr_thread_comments_resolves_pending CHECK (pending OR NOT resolves);

UPDATE schema_info SET version = 138;
