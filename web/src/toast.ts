// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Failed htmx requests show a toast with a retry button in the page's
// status region. A 401 sends the user to the login page.
import { t } from "./i18n/en";

interface ErrorDetail {
  ctx?: { response?: { status?: number }; sourceElement?: Element };
}

function region(): HTMLElement | null {
  return document.getElementById("toasts");
}

export function showToast(message: string, retry?: () => void): void {
  const host = region();
  if (!host) return;
  const toast = document.createElement("div");
  toast.className = "toast toast-error";
  toast.setAttribute("role", "alert");
  const text = document.createElement("p");
  text.textContent = message;
  toast.append(text);
  if (retry) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "button";
    btn.textContent = t("retry");
    btn.addEventListener("click", () => {
      toast.remove();
      retry();
    });
    toast.append(btn);
  }
  const close = document.createElement("button");
  close.type = "button";
  close.className = "button button-quiet";
  close.textContent = t("dismiss");
  close.addEventListener("click", () => toast.remove());
  toast.append(close);
  host.append(toast);
}

export function installErrorToasts(): void {
  const onError = (evt: Event) => {
    const detail = (evt as CustomEvent<ErrorDetail>).detail;
    const status = detail?.ctx?.response?.status;
    if (status === 401) {
      const next = encodeURIComponent(location.pathname + location.search);
      location.assign(`/-/login?next=${next}`);
      return;
    }
    const source = detail?.ctx?.sourceElement;
    const retry =
      source instanceof HTMLElement && source.isConnected
        ? () => (window as unknown as { htmx: { trigger(e: Element, n: string): void } }).htmx.trigger(source, "click")
        : undefined;
    showToast(status ? t("requestFailedStatus", String(status)) : t("requestFailed"), retry);
  };
  document.addEventListener("htmx:response:error", onError);
  document.addEventListener("htmx:error", onError);
}
