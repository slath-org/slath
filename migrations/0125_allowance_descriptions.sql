-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What an allowance's finding is, in words ("OpenSSH private key"), as
-- the scan named it; older allowances fall back to the rule's name.

ALTER TABLE secret_allowances ADD COLUMN description text NULL
    CHECK (char_length(description) <= 200);

UPDATE schema_info SET version = 125;
