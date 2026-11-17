// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: Apache-2.0
//
// The version switcher of the published documentation: the link to the
// list of versions becomes a <select> of them (from versions.json) that
// opens the same page in the chosen version. Without this script, or
// where the list can't be read, the link stays. Local documentation has
// no switcher, so this does nothing there.
(() => {
  const link = document.querySelector("a[data-versions]");
  if (!link) return;
  const current = link.dataset.current;
  fetch(link.dataset.versions)
    .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
    .then((list) => {
      const versions = Array.isArray(list.versions) ? list.versions : [];
      if (versions.length < 2) return;
      const label = document.createElement("label");
      label.textContent = "Version ";
      const select = document.createElement("select");
      for (const v of versions) {
        const o = document.createElement("option");
        o.value = v;
        o.textContent = v;
        o.selected = v === current;
        select.append(o);
      }
      select.addEventListener("change", () => {
        const here = location.pathname;
        const at = here.lastIndexOf(`/${current}/`);
        if (at < 0) return;
        location.pathname = `${here.slice(0, at)}/${select.value}/${here.slice(at + current.length + 2)}`;
      });
      label.append(select);
      link.replaceWith(label);
    })
    .catch(() => {});
})();
