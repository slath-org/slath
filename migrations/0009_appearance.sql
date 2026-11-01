-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Appearance settings next to `theme`: layout density and the code font
-- size in pixels.

UPDATE schema_info SET version = 9;

ALTER TABLE users
    ADD COLUMN density text NOT NULL DEFAULT 'comfortable'
        CHECK (density IN ('comfortable', 'compact')),
    ADD COLUMN code_font_size smallint NOT NULL DEFAULT 14
        CHECK (code_font_size BETWEEN 12 AND 20);
