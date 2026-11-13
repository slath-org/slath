-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Stopping a bot: it's suspended with its credentials removed, and only a
-- stopped bot can be re-enabled by its managers.

ALTER TABLE users ADD COLUMN stopped_at timestamptz NULL;
ALTER TABLE users ADD COLUMN stopped_by uuid NULL REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE users ADD CONSTRAINT users_stopped_bots CHECK (stopped_at IS NULL OR (kind = 'bot' AND state = 'suspended'));

UPDATE schema_info SET version = 130;
