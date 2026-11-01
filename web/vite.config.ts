// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Builds web/src into web/dist with stable file names. The server embeds
// web/dist and serves each file under /-/assets/<content-hash>/<name>, so
// cache busting comes from the server, not from Vite's file names.
import { defineConfig } from "vite";

export default defineConfig({
  base: "./",
  publicDir: false,
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
    target: "es2022",
    modulePreload: false,
    assetsInlineLimit: 0,
    rolldownOptions: {
      input: {
        app: "src/main.ts",
        style: "src/app.css",
      },
      output: {
        entryFileNames: "[name].js",
        chunkFileNames: "[name].js",
        assetFileNames: "[name][extname]",
      },
    },
  },
});
