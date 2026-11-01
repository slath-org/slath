// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Passkeys (WebAuthn). A form with `data-passkey="<options URL>"` gets its
// submit button enabled: on submit, the script asks the server for
// options, lets the browser create or use a passkey, puts the result into
// the form's `credential` and `ceremony` fields, and submits the form. The
// server verifies everything; without JavaScript or WebAuthn the forms stay
// hidden (`hidden` attribute) and the other sign-in methods remain.

import { csrfToken } from "./csrf";
import { t } from "./i18n/en";

type Json = Record<string, unknown>;

function b64urlToBytes(s: string): ArrayBuffer {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const bin = atob((s + pad).replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

function bytesToB64url(b: ArrayBuffer | null | undefined): string | undefined {
  if (!b) return undefined;
  const bytes = new Uint8Array(b);
  let bin = "";
  for (const x of bytes) bin += String.fromCharCode(x);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function decodeIds(list: unknown): unknown {
  if (!Array.isArray(list)) return list;
  return list.map((c: Json) => ({ ...c, id: b64urlToBytes(String(c.id)) }));
}

/** Server options (base64url strings) to what the browser API takes. */
function toCreateOptions(o: Json): CredentialCreationOptions {
  const pk = { ...(o.publicKey as Json) };
  pk.challenge = b64urlToBytes(String(pk.challenge));
  const user = { ...(pk.user as Json) };
  user.id = b64urlToBytes(String(user.id));
  pk.user = user;
  pk.excludeCredentials = decodeIds(pk.excludeCredentials);
  return { publicKey: pk as unknown as PublicKeyCredentialCreationOptions };
}

function toGetOptions(o: Json): CredentialRequestOptions {
  const pk = { ...(o.publicKey as Json) };
  pk.challenge = b64urlToBytes(String(pk.challenge));
  pk.allowCredentials = decodeIds(pk.allowCredentials);
  return {
    publicKey: pk as unknown as PublicKeyCredentialRequestOptions,
    mediation: (o.mediation as CredentialMediationRequirement | undefined) ?? undefined,
  };
}

/** A credential as the server reads it (base64url fields). */
function serialize(c: PublicKeyCredential): Json {
  const r = c.response as AuthenticatorAttestationResponse & AuthenticatorAssertionResponse;
  const response: Json = { clientDataJSON: bytesToB64url(r.clientDataJSON) };
  if ("attestationObject" in r && r.attestationObject) {
    response.attestationObject = bytesToB64url(r.attestationObject);
    if (typeof r.getTransports === "function") response.transports = r.getTransports();
  } else {
    response.authenticatorData = bytesToB64url(r.authenticatorData);
    response.signature = bytesToB64url(r.signature);
    const handle = bytesToB64url(r.userHandle);
    if (handle) response.userHandle = handle;
  }
  return {
    id: c.id,
    rawId: bytesToB64url(c.rawId),
    type: c.type,
    response,
    extensions: c.getClientExtensionResults(),
  };
}

function announce(form: HTMLFormElement, message: string): void {
  const status = form.querySelector<HTMLElement>("[data-passkey-status]");
  if (status) status.textContent = message;
}

async function run(form: HTMLFormElement): Promise<void> {
  const url = form.dataset.passkey ?? "";
  const headers: Record<string, string> = { Accept: "application/json" };
  const token = csrfToken();
  if (token) headers["X-CSRF-Token"] = token;
  const res = await fetch(url, { method: "POST", headers, credentials: "same-origin" });
  if (!res.ok) {
    announce(form, t("passkeyFailed"));
    return;
  }
  const { options, ceremony } = (await res.json()) as { options: Json; ceremony: string };
  let cred: Credential | null;
  try {
    cred =
      form.dataset.passkeyMode === "create"
        ? await navigator.credentials.create(toCreateOptions(options))
        : await navigator.credentials.get(toGetOptions(options));
  } catch {
    announce(form, t("passkeyCancelled"));
    return;
  }
  if (!(cred instanceof PublicKeyCredential)) {
    announce(form, t("passkeyCancelled"));
    return;
  }
  const set = (name: string, value: string) => {
    const input = form.querySelector<HTMLInputElement>(`input[name="${name}"]`);
    if (input) input.value = value;
  };
  set("ceremony", ceremony);
  set("credential", JSON.stringify(serialize(cred)));
  form.dataset.passkeyReady = "1";
  form.requestSubmit();
}

export function installPasskeys(): void {
  if (!window.PublicKeyCredential || !navigator.credentials) return;
  for (const form of document.querySelectorAll<HTMLFormElement>("form[data-passkey]")) {
    form.hidden = false;
    form.addEventListener("submit", (evt) => {
      if (form.dataset.passkeyReady === "1") return;
      evt.preventDefault();
      if (!form.reportValidity()) return;
      void run(form);
    });
  }
}
