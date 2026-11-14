// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Avatars drawn in the browser, for names added after the page loaded
// (chips in a field for several names): the uploaded image, or the same
// pattern the server draws from the name (a symmetric 5x5 pattern from
// the SHA-256 of the lowercased name, in one of eight theme tones).

const SVG = "http://www.w3.org/2000/svg";

/** The SVG path of the pattern for `name`, and its tone (0-7). */
export async function pattern(name: string): Promise<{ tone: number; path: string }> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(name.toLowerCase())));
  let path = "";
  for (let row = 0; row < 5; row++) {
    for (let col = 0; col < 3; col++) {
      if (((bytes[1 + row] >> col) & 1) === 0) continue;
      for (const c of col === 2 ? [2] : [col, 4 - col]) path += `M${c + 1} ${row + 1}h1v1h-1z`;
    }
  }
  return { tone: bytes[0] % 8, path };
}

/** The avatar element for `name`: the uploaded image at `url`, if any. */
export async function avatar(name: string, url: string | null): Promise<Element> {
  if (url) {
    const img = document.createElement("img");
    img.className = "avatar";
    img.src = url;
    img.alt = "";
    img.width = 20;
    img.height = 20;
    return img;
  }
  const { tone, path } = await pattern(name);
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("class", `avatar avatar-generated avatar-tone-${tone}`);
  svg.setAttribute("viewBox", "0 0 7 7");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  const p = document.createElementNS(SVG, "path");
  p.setAttribute("d", path);
  svg.append(p);
  return svg;
}
