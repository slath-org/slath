// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content trust: a work order says whose text and merge requests its task
//! token reads (`all`, `members` by default, or `writers`).

use serde_json::json;

use crate::work_orders::World;

#[test]
fn conf_agent_19_orders_record_their_content_trust() {
    let w = World::start();
    let r = w.delegate(&w.bob, json!({}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["content_trust"], "members");
    let r = w.api(&w.bob, "POST", "/work-orders/1/stop", None);
    assert!(r.status < 300, "{}", r.body);

    let r = w.delegate(&w.bob, json!({"content_trust": "trusted"}));
    assert_eq!(r.status, 422, "{}", r.body);
    for level in ["writers", "all"] {
        let r = w.delegate(&w.bob, json!({"content_trust": level}));
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        let r = w.api(&w.carol, "GET", &format!("/work-orders/{n}"), None);
        assert_eq!(r.json()["content_trust"], level);
        let r = w.api(&w.bob, "POST", &format!("/work-orders/{n}/stop"), None);
        assert!(r.status < 300, "{}", r.body);
    }
}
