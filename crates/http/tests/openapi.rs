// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The committed `docs/api/openapi.json` matches the API's generated
//! document. Regenerate with `SLATH_UPDATE_SNAPSHOTS=1 just api-check`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
fn committed_openapi_document_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/openapi.json");
    let doc = slath_http::api::openapi();
    let json = serde_json::to_string_pretty(&doc).unwrap() + "\n";
    if std::env::var_os("SLATH_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &json).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == json,
        "docs/api/openapi.json is out of date; run `SLATH_UPDATE_SNAPSHOTS=1 just api-check`"
    );
}

#[test]
fn every_operation_documents_its_errors() {
    let doc = serde_json::to_value(slath_http::api::openapi()).unwrap();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let responses = op["responses"].as_object().unwrap();
            // Token creation is deliberately browser-only.
            if !(path == "/api/v1/user/tokens" && method == "post") {
                assert!(
                    responses
                        .keys()
                        .any(|k| k.starts_with('2') || k.starts_with('3')),
                    "{method} {path}: no success response"
                );
            }
            if path != "/api/v1/version" && path != "/api/v1/repos" && method == "get" {
                assert!(
                    responses.keys().any(|k| k == "404" || k == "401"),
                    "{method} {path}: no 401/404 documented"
                );
            }
        }
    }
}
