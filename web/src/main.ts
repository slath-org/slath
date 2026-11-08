// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Entry point of the UI bundle: htmx 4 with the hx-csp extension, plus the
// small modules every page needs. Pages work without this script; it only
// enhances them.
import "htmx.org";
import "htmx.org/dist/ext/hx-csp.js";
import { installCsrf } from "./csrf";
import { installA11ySwaps } from "./a11y";
import { installErrorToasts, installToastButtons } from "./toast";
import { installTheme } from "./theme";
import { installMenus } from "./menu";
import { installDialogs } from "./dialog";
import { installPasskeys } from "./passkeys";
import { installEditors } from "./editor";
import { installSuggestions } from "./suggest";
import { installCopyButtons } from "./copy";

installCsrf();
installA11ySwaps();
installErrorToasts();
installToastButtons();
installTheme();
installMenus();
installDialogs();
installPasskeys();
installEditors();
installSuggestions();
installCopyButtons();
