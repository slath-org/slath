// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Replaces micromark's `decode-named-character-reference` (its browser
// build uses innerHTML, its other build a 12 KiB table): the browser decodes
// one named character reference, and only names made of letters and digits
// ever reach the Trusted Types sink.
import { trustedHTML } from "./tt";

const element = document.createElement("i");

export function decodeNamedCharacterReference(value: string): string | false {
  if (!/^[A-Za-z0-9]{1,32}$/.test(value)) return false;
  const reference = `&${value};`;
  element.innerHTML = trustedHTML(reference);
  const character = element.textContent ?? "";
  // Without the semicolon the reference wasn't a complete one.
  if (character.charCodeAt(character.length - 1) === 59 && value !== "semi") return false;
  return character === reference ? false : character;
}
