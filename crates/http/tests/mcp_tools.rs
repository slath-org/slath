// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The MCP tool list: every tool is a `GET` operation of the API, and the
//! committed `docs/api/mcp-tools.json` matches the code. Regenerate with
//! `SLATH_UPDATE_SNAPSHOTS=1 just api-check`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use slath_http::api::mcp::{TOOL_DEFS, TOOLS};

#[test]
fn conf_agent_04_every_tool_is_an_api_read() {
    let built: Vec<_> = TOOL_DEFS.iter().map(|t| t.name).collect();
    let wanted: Vec<_> = TOOLS.iter().map(|(n, _)| *n).collect();
    assert_eq!(built, wanted, "a tool's operation is missing or not a GET");
    for t in TOOL_DEFS.iter() {
        let d = &t.definition;
        assert!(
            !d["description"].as_str().unwrap().starts_with('\n'),
            "{}: its operation has no summary",
            t.name
        );
        assert_eq!(d["annotations"]["readOnlyHint"], true, "{}", t.name);
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        for (name, schema) in d["inputSchema"]["properties"].as_object().unwrap() {
            let ty = schema["type"].as_str();
            assert!(
                matches!(ty, Some("string" | "integer" | "boolean")),
                "{}.{name}: unexpected schema {schema}",
                t.name
            );
        }
    }
}

#[test]
fn conf_agent_04_committed_tool_schemas_are_current() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/mcp-tools.json");
    let tools: Vec<_> = TOOL_DEFS.iter().map(|t| t.definition.clone()).collect();
    let json = serde_json::to_string_pretty(&serde_json::json!({ "tools": tools })).unwrap() + "\n";
    if std::env::var_os("SLATH_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &json).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == json,
        "docs/api/mcp-tools.json is out of date; run `SLATH_UPDATE_SNAPSHOTS=1 just api-check`"
    );
}
