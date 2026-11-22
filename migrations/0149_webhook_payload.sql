-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What a webhook's deliveries carry: the full payload, or only the event,
-- IDs, names, and API URLs, so no titles, descriptions, or comments reach
-- the receiver's delivery logs. Existing webhooks keep full payloads.

ALTER TABLE webhooks
    ADD COLUMN payload text NOT NULL DEFAULT 'full' CHECK (payload IN ('full', 'ids'));

UPDATE schema_info SET version = 149;
