// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The time zone setting proposes this device's zone: a
// `<select data-propose-zone>` with no zone saved (`data-unset`) selects
// the device's zone if it's in the list, and its hint
// (`[data-zone-hint]`) says so. Nothing is saved until the form is.
// Without this script the list starts at UTC.
import { t } from "./i18n/en";

export function installZoneProposal(): void {
  const propose = (root: ParentNode) => {
    root
      .querySelectorAll<HTMLSelectElement>("select[data-propose-zone][data-unset]")
      .forEach((select) => {
        const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
        if (!zone || zone === "UTC" || select.value === zone) return;
        const option = Array.from(select.options).find((o) => o.value === zone);
        if (!option) return;
        select.value = zone;
        delete select.dataset.unset;
        const hint = select.form?.querySelector<HTMLElement>("[data-zone-hint]");
        if (hint) {
          hint.textContent = t("zoneProposed", zone);
          hint.hidden = false;
        }
      });
  };
  propose(document);
  document.addEventListener("htmx:after:swap", (e) => {
    if (e.target instanceof Element) propose(e.target);
  });
}
