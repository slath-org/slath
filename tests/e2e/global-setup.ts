// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Starts one Slath server (fresh database and data directory) for the whole
// run via the `slath-e2e-server` helper, unless SLATH_E2E_URL is already set.
import { spawn } from "node:child_process";
import { resolve } from "node:path";

export default async function globalSetup(): Promise<() => Promise<void>> {
  if (process.env.SLATH_E2E_URL) {
    return async () => {};
  }
  const root = resolve(import.meta.dirname, "../..");
  const bin = process.env.SLATH_E2E_SERVER ?? resolve(root, "target/debug/slath-e2e-server");
  const child = spawn(bin, [], { cwd: root, stdio: ["pipe", "pipe", "inherit"] });
  const url = await new Promise<string>((ok, fail) => {
    let buf = "";
    const timer = setTimeout(() => fail(new Error("slath-e2e-server did not start in 60 s")), 60_000);
    child.on("exit", (code) => fail(new Error(`slath-e2e-server exited with ${code}`)));
    child.stdout.on("data", (chunk: Buffer) => {
      buf += chunk.toString();
      const m = buf.match(/SLATH_E2E_URL=(\S+)/);
      if (m) {
        clearTimeout(timer);
        ok(m[1]);
      }
    });
  });
  process.env.SLATH_E2E_URL = url;
  return async () => {
    child.stdin.end();
    await new Promise((r) => child.on("exit", r));
  };
}
