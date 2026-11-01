// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Contract tests: real API responses are checked against the committed
//! OpenAPI document (`docs/api/openapi.json`).

use std::sync::OnceLock;

use crate::http::Resp;

fn doc() -> &'static serde_json::Value {
    static DOC: OnceLock<serde_json::Value> = OnceLock::new();
    DOC.get_or_init(|| {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/api/openapi.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    })
}

/// Asserts that `resp` is documented for `method template` and that its
/// JSON body matches the documented schema.
pub fn check(method: &str, template: &str, resp: &Resp) {
    let doc = doc();
    let op = &doc["paths"][template][method.to_ascii_lowercase()];
    assert!(op.is_object(), "{method} {template} is not documented");
    let status = resp.status.to_string();
    let r = &op["responses"][&status];
    assert!(
        r.is_object(),
        "{method} {template}: status {status} is not documented (body: {})",
        resp.body
    );
    let Some((_, media)) = r["content"].as_object().and_then(|c| c.iter().next()) else {
        return;
    };
    let schema = &media["schema"];
    // Validate against the schema with the whole document as the root, so
    // "#/components/schemas/…" references resolve.
    let mut wrapped = serde_json::json!({ "components": doc["components"].clone() });
    for (k, v) in schema.as_object().unwrap() {
        wrapped
            .as_object_mut()
            .unwrap()
            .insert(k.clone(), v.clone());
    }
    let validator = jsonschema::draft202012::new(&wrapped)
        .unwrap_or_else(|e| panic!("{method} {template}: invalid schema: {e}"));
    let body = resp.json();
    let errors: Vec<String> = validator
        .iter_errors(&body)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{method} {template} {status}: response doesn't match the schema: {errors:?}\n{body}"
    );
}
