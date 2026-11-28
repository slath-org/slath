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
            let browser_only = method == "post"
                && (path == "/api/v1/user/tokens" || path == "/api/v1/bots/{name}/tokens");
            if !browser_only {
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

#[test]
fn path_parameters_are_in_the_path() {
    let doc = serde_json::to_value(slath_http::api::openapi()).unwrap();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            for p in op["parameters"].as_array().into_iter().flatten() {
                let name = p["name"].as_str().unwrap();
                if p["in"] == "path" {
                    assert!(
                        path.contains(&format!("{{{name}}}")),
                        "{method} {path}: `{name}` is a query parameter documented as a path parameter"
                    );
                }
            }
        }
    }
}

/// Fields that hold free text in response objects.
const TEXT_FIELDS: &[&str] = &[
    "title",
    "body",
    "body_html",
    "description",
    "description_html",
    "message",
    "category",
    "display_name",
    "text",
];

/// Free-text fields without origin labels, and why (spec: origin labels).
const NOT_USER_TEXT: &[(&str, &str, &str)] = &[
    (
        "PathClassError",
        "message",
        "Slath's own wording about the default branch's .slath/paths.toml, which is Git content",
    ),
    (
        "FactSecret",
        "description",
        "a push protection rule's description, from the built-in or the operator's rule set",
    ),
    (
        "CommitDetail",
        "message",
        "Git content: commit messages have no Slath author",
    ),
    (
        "IterationInterdiff",
        "text",
        "Git content: a range-diff of commits",
    ),
    (
        "StatusAnnotation",
        "category",
        "nested in CommitStatus, which carries the markers",
    ),
    (
        "StatusAnnotation",
        "message",
        "nested in CommitStatus, which carries the markers",
    ),
    (
        "BriefItemBody",
        "text",
        "nested in BriefBody, which carries the markers",
    ),
    (
        "HeldFinding",
        "description",
        "Slath's own text: a push protection rule's description",
    ),
    (
        "User",
        "display_name",
        "profile text: belongs to no repository",
    ),
    (
        "Bot",
        "display_name",
        "profile text: belongs to no repository",
    ),
    (
        "Org",
        "display_name",
        "profile text: belongs to no repository",
    ),
    (
        "Org",
        "description",
        "profile text: belongs to no repository",
    ),
    ("SshKey", "title", "a key's title, seen only by its owner"),
    (
        "DeployKey",
        "title",
        "a key's title, seen only by repository administrators",
    ),
];

/// Schemas reachable from a success response.
fn response_schemas(doc: &serde_json::Value) -> std::collections::BTreeSet<String> {
    fn walk(
        v: &serde_json::Value,
        doc: &serde_json::Value,
        seen: &mut std::collections::BTreeSet<String>,
    ) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(r) = m.get("$ref").and_then(|r| r.as_str()) {
                    let name = r.rsplit('/').next().unwrap().to_owned();
                    if seen.insert(name.clone()) {
                        walk(&doc["components"]["schemas"][&name], doc, seen);
                    }
                }
                for x in m.values() {
                    walk(x, doc, seen);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, doc, seen)),
            _ => {}
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in doc["paths"].as_object().unwrap().values() {
        for op in item.as_object().unwrap().values() {
            for (code, r) in op["responses"].as_object().unwrap() {
                if code.starts_with('2') {
                    walk(r, doc, &mut seen);
                }
            }
        }
    }
    seen
}

#[test]
fn conf_agent_12_every_user_text_field_is_marked() {
    let doc = serde_json::to_value(slath_http::api::openapi()).unwrap();
    let mut missing = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    for name in response_schemas(&doc) {
        let Some(props) = doc["components"]["schemas"][&name]["properties"].as_object() else {
            continue;
        };
        let marked = props.contains_key("untrusted_content") && props.contains_key("origin");
        for field in TEXT_FIELDS.iter().filter(|f| props.contains_key(**f)) {
            if let Some(e) = NOT_USER_TEXT
                .iter()
                .find(|(s, f, _)| *s == name && f == field)
            {
                used.insert((e.0, e.1));
            } else if !marked {
                missing.push(format!("{name}.{field}"));
            }
        }
        if marked {
            let origin = &props["origin"];
            assert!(
                origin.get("$ref").and_then(|r| r.as_str())
                    == Some("#/components/schemas/TextOrigin"),
                "{name}.origin isn't a TextOrigin: {origin}"
            );
        }
    }
    assert!(
        missing.is_empty(),
        "user-generated text without origin labels: {missing:?}"
    );
    let stale: Vec<_> = NOT_USER_TEXT
        .iter()
        .filter(|(s, f, _)| !used.contains(&(*s, *f)))
        .collect();
    assert!(
        stale.is_empty(),
        "exclusions that match no response field: {stale:?}"
    );
    let origins = &doc["components"]["schemas"]["TextOrigin"]["enum"];
    assert_eq!(
        origins,
        &serde_json::json!([
            "anonymous",
            "outside_contributor",
            "bot",
            "member",
            "writer"
        ])
    );
}
