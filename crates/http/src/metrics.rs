// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A small Prometheus registry. Labels are bounded by construction: route
//! patterns (never concrete paths), methods, status classes, and fixed
//! reason strings. No repository names or user IDs ever become labels.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::Duration;

/// Upper bounds of the request duration histogram, in seconds.
const BUCKETS: [f64; 10] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0];

#[derive(Debug, Default)]
struct Histogram {
    counts: [u64; BUCKETS.len()],
    count: u64,
    sum: f64,
}

#[derive(Debug, Default)]
struct Inner {
    /// `(route, method, status_class)` → count.
    requests: BTreeMap<(String, String, &'static str), u64>,
    durations: BTreeMap<String, Histogram>,
    /// `(name, labels)` → value, for counters other modules record.
    counters: BTreeMap<(&'static str, String), u64>,
    gauges: BTreeMap<(&'static str, String), i64>,
}

#[derive(Debug, Default)]
pub struct Metrics {
    inner: Mutex<Inner>,
}

fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

impl Metrics {
    pub fn observe_http(&self, route: &str, method: &str, status: u16, elapsed: Duration) {
        let Ok(mut m) = self.inner.lock() else { return };
        *m.requests
            .entry((route.to_owned(), method.to_owned(), status_class(status)))
            .or_default() += 1;
        let h = m.durations.entry(route.to_owned()).or_default();
        let secs = elapsed.as_secs_f64();
        for (i, b) in BUCKETS.iter().enumerate() {
            if secs <= *b {
                h.counts[i] += 1;
            }
        }
        h.count += 1;
        h.sum += secs;
    }

    /// Increments a counter. `labels` must come from a fixed set of values,
    /// formatted as `key="value",…`.
    pub fn inc(&self, name: &'static str, labels: &str) {
        if let Ok(mut m) = self.inner.lock() {
            *m.counters.entry((name, labels.to_owned())).or_default() += 1;
        }
    }

    pub fn set_gauge(&self, name: &'static str, labels: &str, value: i64) {
        if let Ok(mut m) = self.inner.lock() {
            m.gauges.insert((name, labels.to_owned()), value);
        }
    }

    /// The text exposition format.
    #[must_use]
    pub fn render(&self) -> String {
        let Ok(m) = self.inner.lock() else {
            return String::new();
        };
        let mut out = String::new();
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        out.push_str("# TYPE slath_http_requests_total counter\n");
        for ((route, method, class), n) in &m.requests {
            let _ = writeln!(
                out,
                "slath_http_requests_total{{route=\"{}\",method=\"{}\",status_class=\"{class}\"}} {n}",
                esc(route),
                esc(method)
            );
        }
        out.push_str("# TYPE slath_http_request_duration_seconds histogram\n");
        for (route, h) in &m.durations {
            let r = esc(route);
            for (i, b) in BUCKETS.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "slath_http_request_duration_seconds_bucket{{route=\"{r}\",le=\"{b}\"}} {}",
                    h.counts[i]
                );
            }
            let _ = writeln!(
                out,
                "slath_http_request_duration_seconds_bucket{{route=\"{r}\",le=\"+Inf\"}} {}",
                h.count
            );
            let _ = writeln!(
                out,
                "slath_http_request_duration_seconds_sum{{route=\"{r}\"}} {}",
                h.sum
            );
            let _ = writeln!(
                out,
                "slath_http_request_duration_seconds_count{{route=\"{r}\"}} {}",
                h.count
            );
        }
        let mut last = "";
        for ((name, labels), n) in &m.counters {
            if *name != last {
                let _ = writeln!(out, "# TYPE {name} counter");
                last = name;
            }
            let _ = writeln!(out, "{name}{{{labels}}} {n}");
        }
        last = "";
        for ((name, labels), n) in &m.gauges {
            if *name != last {
                let _ = writeln!(out, "# TYPE {name} gauge");
                last = name;
            }
            let _ = writeln!(out, "{name}{{{labels}}} {n}");
        }
        out
    }
}

/// The metrics listener's router (`GET /metrics`).
pub fn router(metrics: std::sync::Arc<Metrics>) -> axum::Router {
    axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let m = std::sync::Arc::clone(&metrics);
            async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; version=0.0.4",
                    )],
                    m.render(),
                )
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_and_histograms() {
        let m = Metrics::default();
        m.observe_http("/{ns}", "GET", 200, Duration::from_millis(7));
        m.observe_http("/{ns}", "GET", 404, Duration::from_millis(30));
        m.inc("slath_hook_denials_total", "reason=\"reserved_ref\"");
        let text = m.render();
        assert!(text.contains(
            "slath_http_requests_total{route=\"/{ns}\",method=\"GET\",status_class=\"2xx\"} 1"
        ));
        assert!(text.contains("le=\"0.01\"} 1"));
        assert!(text.contains("_count{route=\"/{ns}\"} 2"));
        assert!(text.contains("slath_hook_denials_total{reason=\"reserved_ref\"} 1"));
    }
}
