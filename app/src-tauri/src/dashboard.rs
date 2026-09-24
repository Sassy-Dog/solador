//! Compact views over the same readings as the detailed panels. This module
//! owns scope, severity, truncation, and copy; the webview only arranges them.
//! Attention is computed once per source, before tile visibility or filtering.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use store::{DashboardLayout, DashboardTile};
use viewmodel::color;

pub const SOURCES: [(&str, &str); 9] = [
    ("hosts", "Machines"),
    ("ghWorkflows", "GitHub repos"),
    ("ghRunners", "Runners"),
    ("services", "Service health"),
    ("sentryCrons", "Scheduled jobs"),
    ("containers", "Containers / VMs"),
    ("claudeUsage", "Usage"),
    ("azureCost", "Azure Cost"),
    ("openclawAgents", "OpenClaw"),
];

pub fn default_layout() -> DashboardLayout {
    DashboardLayout {
        revision: 0,
        tiles: SOURCES[..5]
            .iter()
            .enumerate()
            .map(|(i, (source, title))| DashboardTile {
                id: format!("overview-{source}"),
                source: (*source).into(),
                title: (*title).into(),
                scope: if *source == "sentryCrons" {
                    "active"
                } else {
                    "all"
                }
                .into(),
                presentation: "summary".into(),
                width: if i < 2 { "medium" } else { "small" }.into(),
                hidden: false,
            })
            .collect(),
    }
}

/// Reject the entire edit rather than silently broadening an invalid scope or
/// dropping a tile. Unknown resource IDs remain valid, so a disconnected or
/// removed resource does not turn its tile into an unfiltered one.
pub fn validate(layout: &DashboardLayout) -> Result<(), String> {
    if layout.tiles.len() > 48 {
        return Err("A dashboard can contain up to 48 tiles.".into());
    }
    let mut ids = BTreeSet::new();
    for tile in &layout.tiles {
        if tile.id.is_empty()
            || tile.id.len() > 80
            || !tile
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !ids.insert(&tile.id)
        {
            return Err("Every tile needs its own valid identity.".into());
        }
        if !SOURCES.iter().any(|(id, _)| *id == tile.source) {
            return Err("That tile source is not supported.".into());
        }
        if tile.title.trim().is_empty()
            || tile.title.chars().count() > 64
            || tile.title.chars().any(char::is_control)
        {
            return Err("Use a tile name between 1 and 64 characters.".into());
        }
        if !["summary", "detailed"].contains(&tile.presentation.as_str())
            || !["small", "medium", "wide"].contains(&tile.width.as_str())
        {
            return Err("Choose one of the supported tile sizes and presentations.".into());
        }
        let fixed = fixed_scopes(&tile.source)
            .iter()
            .any(|(v, _)| *v == tile.scope);
        let resource = tile.scope.strip_prefix("item:").is_some_and(|id| {
            !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
        });
        if !fixed && !resource {
            return Err("Choose a supported scope for this tile.".into());
        }
    }
    Ok(())
}

fn fixed_scopes(source: &str) -> Vec<(&'static str, &'static str)> {
    let mut choices = vec![("all", "All resources"), ("attention", "Needs attention")];
    match source {
        "hosts" => choices.extend([("local", "This machine"), ("remote", "Remote machines")]),
        "ghWorkflows" => choices.push(("healthy", "Healthy repos")),
        "ghRunners" => choices.extend([
            ("MACOS", "macOS runners"),
            ("LINUX", "Linux runners"),
            ("WINDOWS", "Windows runners"),
        ]),
        "sentryCrons" => choices.push(("active", "Active issues")),
        "claudeUsage" => {
            choices.extend([("claude", "Claude Code"), ("providers", "Cloud providers")])
        }
        _ => (),
    }
    choices
}

fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}
fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map(Vec::as_slice).unwrap_or_default()
}
fn red(v: &Value) -> bool {
    v.as_str() == Some(color::hex(color::RED).as_str())
}
fn amber(v: &Value) -> bool {
    v.as_str() == Some(color::hex(color::AMBER).as_str())
}
fn tint(v: &Value) -> Value {
    if v.is_string() {
        v.clone()
    } else {
        json!(color::hex(color::MUTED))
    }
}
fn row(id: &str, label: &str, value: &str, color: Value, attention: bool) -> Value {
    json!({"id": id, "label": label, "value": value, "color": color, "valueColor": color, "attention": attention, "detail": "", "scopes": [], "metrics": [], "url": null})
}
fn add_scope(row: &mut Value, scope: &str) {
    row["scopes"]
        .as_array_mut()
        .expect("row scopes")
        .push(json!(scope));
}
fn field(label: &str, value: &Value) -> Value {
    json!({"label":label, "value":value, "fraction":null})
}

fn host_rows(p: &Value) -> Vec<Value> {
    list(p, "hosts")
        .iter()
        .map(|h| {
            let name = h["hostName"]
                .as_str()
                .or(h["error"]["hostName"].as_str())
                .unwrap_or("Machine");
            let connection = string(&h["connection"], "state");
            let pending = connection == "connecting";
            let down = !h["error"].is_null();
            let problem = !pending && (down || connection != "live");
            let volumes = list(h, "volumes");
            let volume_warning = volumes
                .iter()
                .find(|v| red(&v["tint"]))
                .or_else(|| volumes.iter().find(|v| amber(&v["tint"])));
            let metric_colors = [&h["cpuValueColor"], &h["memValueColor"], &h["thermalColor"]];
            let metric_warning = metric_colors.iter().any(|v| red(v) || amber(v));
            let status = if pending {
                "Connecting".to_owned()
            } else if down {
                "Unreachable".to_owned()
            } else if problem {
                "Stale / unavailable".into()
            } else if let Some(v) = volume_warning {
                v["fraction"].as_f64().map_or_else(
                    || format!("{} · check disk space", string(v, "mount")),
                    |fraction| format!("{} at {:.0}%", string(v, "mount"), fraction * 100.0),
                )
            } else if metric_warning {
                "Check metrics".into()
            } else {
                "Connected".into()
            };
            let mut r = row(
                string(h, "id"),
                name,
                &status,
                if problem || pending {
                    tint(&h["connection"]["color"])
                } else if metric_colors.iter().any(|v| red(v)) {
                    json!(color::hex(color::RED))
                } else if let Some(v) = volume_warning {
                    tint(&v["tint"])
                } else if metric_warning {
                    json!(color::hex(color::AMBER))
                } else {
                    tint(&h["connection"]["color"])
                },
                problem || volume_warning.is_some() || metric_warning,
            );
            add_scope(
                &mut r,
                if h["id"] == "local" {
                    "local"
                } else {
                    "remote"
                },
            );
            r["detail"] = json!(if down {
                string(&h["error"], "message").to_owned()
            } else {
                [string(h, "cpuModel"), string(&h["connection"], "message")]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            });
            // Keep the same reading slots through connection transitions. Unknown
            // values preserve the layout without presenting old data as current.
            let reading = |key: &str| if down { &Value::Null } else { &h[key] };
            r["metrics"] = json!([
                field("CPU", reading("cpuValue")),
                field("RAM", reading("memValue"))
            ]);
            r["details"] = json!([
                field("Disk read", reading("diskRead")),
                field("Disk write", reading("diskWrite")),
                field("Network down", reading("netDown")),
                field("Network up", reading("netUp")),
                field("GPU", reading("gpuValue")),
                field("Thermal", reading("thermalText")),
            ]);
            if !down {
                r["volumes"] = h["volumes"].clone();
            }
            r
        })
        .collect()
}

/// The Repos columns a summary row carries *on* the row, between the name and
/// the status, as `7 issues · 3 ready · 2 PRs`: the backlog at a glance. Each
/// entry is the table's header and the two words the strip prints for it —
/// one thing and several — chosen here by the value, so the frontend never
/// pluralises. Named by header so the detailed table stays the one place a
/// column's position is decided.
///
/// On the row rather than under it (the way a machine's CPU/RAM sit) because
/// five two-line rows are what stops the default overview fitting a 1024×768
/// laptop — the e2e suite measures that.
const ROW_COUNTS: [(&str, &str, &str); 3] = [
    (crate::github::COL_ISSUES, "issue", "issues"),
    (crate::github::COL_READY, "ready", "ready"),
    (crate::github::COL_PRS, "PR", "PRs"),
];

/// The widest word each strip column prints, in characters, and the widest
/// status word beside it. `app/ui/dashboard.css` reserves exactly these —
/// `.db-count[data-header=…] > span { width: Nch }` and
/// `.db-item-tabular > .db-value { min-width: 14ch }` — so a longer word
/// here would overflow its slot on every row without moving a number the
/// e2e alignment test could see. The test below is the link.
#[cfg(test)]
const ROW_COUNT_SLOT_CH: [(&str, usize); 3] = [
    (crate::github::COL_ISSUES, 6),
    (crate::github::COL_READY, 5),
    (crate::github::COL_PRS, 3),
];
#[cfg(test)]
const ROW_STATUS_SLOT_CH: usize = 14;

/// One entry of a repo row's count strip: the value verbatim from the cell,
/// the word the strip prints beside it (singular for exactly `1`, plural for
/// everything else including `0` and `—`), and the table header the inspector
/// lists it under so the eight numbers read in one vocabulary there.
fn count(header: &str, one: &str, many: &str, value: &Value) -> Value {
    let word = if value.as_str() == Some("1") {
        one
    } else {
        many
    };
    json!({"label": word, "header": header, "value": value, "fraction": null})
}

fn source_rows(id: &str, p: &Value) -> Vec<Value> {
    match id {
        "hosts" => host_rows(p),
        "ghWorkflows" => list(p, "rows")
            .iter()
            .map(|v| {
                let status = string(v, "statusLabel");
                let mut r = row(
                    string(v, "repo"),
                    string(v, "name"),
                    status,
                    tint(&v["dotColor"]),
                    v["attention"].as_bool().unwrap_or(false),
                );
                r["url"] = v["url"].clone();
                if v["status"] == "healthy" {
                    add_scope(&mut r, "healthy");
                }
                // Every numeric column of the detailed table, keyed by its
                // header. The first column is REPO, which the row's label
                // already carries.
                let cells: Vec<(&str, &Value)> = list(p, "columns")
                    .iter()
                    .skip(1)
                    .zip(list(v, "cells"))
                    .map(|(column, cell)| (string(column, "label"), &cell["text"]))
                    .collect();
                let cell = |label: &str| {
                    cells
                        .iter()
                        .find(|(l, _)| *l == label)
                        .map_or(&Value::Null, |(_, text)| *text)
                };
                r["detail"] = json!(format!(
                    "{} · longest running: {}",
                    string(v, "repo"),
                    cell(crate::github::COL_LONGEST).as_str().unwrap_or("—")
                ));
                // The backlog at a glance rides on the row in every
                // presentation. Verbatim from the cells: an unknown count
                // arrives as the em dash Rust already chose for it, never
                // re-derived here.
                r["counts"] = json!(ROW_COUNTS
                    .iter()
                    .map(|(column, one, many)| count(column, one, many, cell(column)))
                    .collect::<Vec<_>>());
                // …and what is left is Detailed's extra, so the three are
                // never printed twice on one row.
                r["details"] = json!(cells
                    .iter()
                    .filter(|(label, _)| !ROW_COUNTS.iter().any(|(column, _, _)| column == label))
                    .map(|(label, text)| field(label, text))
                    .collect::<Vec<_>>());
                r
            })
            .collect(),
        "ghRunners" => list(p, "rows")
            .iter()
            .map(|v| {
                let key = format!("{}/{}", string(v, "org"), string(v, "name"));
                let mut r = row(
                    &key,
                    string(v, "name"),
                    string(v, "status"),
                    tint(&v["dotColor"]),
                    v["attention"].as_bool().unwrap_or(false),
                );
                r["detail"] = json!(format!("{} · {}", string(v, "org"), string(v, "os")));
                add_scope(&mut r, string(v, "os"));
                r
            })
            .collect(),
        "services" => list(p, "rows")
            .iter()
            .map(|v| {
                let mut r = row(
                    string(v, "id"),
                    string(v, "label"),
                    string(v, "state"),
                    tint(&v["color"]),
                    v["degraded"] == true || v["unknown"] == true || v["readFailed"] == true,
                );
                r["detail"] = v["detail"].clone();
                if v["readFailed"] == true {
                    r["value"] = json!(format!("Last known: {}", string(v, "state")));
                }
                r
            })
            .collect(),
        "sentryCrons" => list(p, "rows")
            .iter()
            .map(|v| {
                let active = v["suppressed"] != true;
                let mut r = row(
                    string(v, "id"),
                    string(v, "label"),
                    string(v, "age"),
                    tint(&v["color"]),
                    active,
                );
                r["valueColor"] = v["ageColor"].clone();
                r["detail"] = v["detail"].clone();
                r["explanation"] = v["title"].clone();
                r["compactDetail"] =
                    json!(string(v, "detail").split(" · ").next().unwrap_or_default());
                if active {
                    add_scope(&mut r, "active");
                }
                r
            })
            .collect(),
        "containers" => list(p, "sections")
            .iter()
            .flat_map(|section| {
                list(section, "rows").iter().map(|v| {
                    let key = format!("{}|{}", string(section, "host"), string(v, "name"));
                    let mut r = row(
                        &key,
                        string(v, "name"),
                        string(v, "status"),
                        tint(&v["dotColor"]),
                        v["attention"].as_bool().unwrap_or(false),
                    );
                    r["detail"] = json!(format!(
                        "{} · {}",
                        string(section, "label"),
                        string(v, "runtime")
                    ));
                    r
                })
            })
            .collect(),
        "claudeUsage" => {
            let mut rows = Vec::new();
            if !string(p, "trailing").is_empty() {
                let mut r = row(
                    "claude-today",
                    "Claude Code",
                    string(p, "trailing"),
                    json!(color::hex(color::MUTED)),
                    false,
                );
                add_scope(&mut r, "claude");
                rows.push(r);
            }
            for v in list(p, "windows") {
                let mut r = row(
                    &format!("claude-{}", string(v, "label")),
                    string(v, "label"),
                    string(v, "value"),
                    tint(&v["valueColor"]),
                    false,
                );
                add_scope(&mut r, "claude");
                rows.push(r);
            }
            for provider in list(p, "providers") {
                for v in list(provider, "rows") {
                    let mut r = row(
                        &format!("{}:{}", string(provider, "id"), string(v, "label")),
                        string(v, "label"),
                        string(v, "value"),
                        tint(&v["valueColor"]),
                        red(&v["valueColor"])
                            || red(&provider["bar"]["color"])
                            || amber(&provider["bar"]["color"]),
                    );
                    if red(&provider["bar"]["color"]) || amber(&provider["bar"]["color"]) {
                        r["color"] = provider["bar"]["color"].clone();
                    }
                    add_scope(&mut r, "providers");
                    r["detail"] = json!(string(provider, "id"));
                    rows.push(r);
                }
            }
            rows
        }
        "azureCost" => {
            let mut rows = Vec::new();
            if let Some(value) = p["headline"]["value"].as_str() {
                let mut r = row(
                    "mtd",
                    "Month to date",
                    value,
                    tint(&p["headline"]["valueColor"]),
                    red(&p["headline"]["valueColor"]),
                );
                r["detail"] = p["headline"]["caption"].clone();
                rows.push(r);
            }
            for v in list(p, "stats") {
                rows.push(row(
                    &format!("cost-{}", string(v, "label")),
                    string(v, "label"),
                    string(v, "value"),
                    tint(&v["valueColor"]),
                    red(&v["valueColor"]),
                ));
            }
            if !p["budget"].is_null() {
                rows.push(row(
                    "budget",
                    string(&p["budget"], "label"),
                    string(&p["budget"], "value"),
                    tint(&p["budget"]["bar"]["color"]),
                    red(&p["budget"]["bar"]["color"]) || amber(&p["budget"]["bar"]["color"]),
                ));
            }
            rows
        }
        "openclawAgents" => {
            let mut rows = Vec::new();
            for runtime in list(p, "runtimes") {
                let runtime_id = string(runtime, "id");
                for v in list(&runtime["agents"], "rows") {
                    let mut r = row(
                        &format!("{runtime_id}:agent:{}", string(v, "id")),
                        string(v, "name"),
                        string(v, "status"),
                        tint(&v["dot"]["color"]),
                        matches!(string(v, "status"), "error" | "unknown"),
                    );
                    r["detail"] = v["detail"].clone();
                    rows.push(r);
                }
                for key in ["connection", "hint", "pairing", "cron"] {
                    let v = &runtime[key];
                    if v.is_null() {
                        continue;
                    }
                    let text = v["text"]
                        .as_str()
                        .or(v["title"].as_str())
                        .or(v["summary"].as_str())
                        .unwrap_or_default();
                    let color = if key == "pairing" || key == "connection" {
                        tint(&v["dotColor"])
                    } else if key == "cron" {
                        tint(&v["dot"]["color"])
                    } else {
                        tint(&v["color"])
                    };
                    let attention = key == "pairing" || red(&color);
                    let mut r = row(&format!("{runtime_id}-{key}"), key, text, color, attention);
                    r["detail"] = if key == "pairing" {
                        v["command"].clone()
                    } else {
                        v["error"]["text"].clone()
                    };
                    rows.push(r);
                }
                for v in list(&runtime["channels"], "rows") {
                    rows.push(row(
                        &format!("{runtime_id}:channel:{}", string(v, "id")),
                        string(v, "name"),
                        string(v, "status"),
                        tint(&v["dot"]["color"]),
                        matches!(string(v, "status"), "error" | "unknown"),
                    ));
                }
            }
            rows
        }
        _ => vec![],
    }
}

fn source_view(id: &str, title: &str, payload: &Value) -> Value {
    let rows = source_rows(id, payload);
    let mut warnings = Vec::new();
    for key in ["footer", "freshness"] {
        if let Some(text) = payload[key]["text"].as_str().filter(|s| !s.is_empty()) {
            warnings.push(json!({"text":text,"color":tint(&payload[key]["color"])}));
        }
    }
    let message = payload["message"]["text"]
        .as_str()
        .or(payload["empty"]["message"].as_str())
        .or(payload["empty"].as_str())
        .unwrap_or_default();
    if red(&payload["message"]["color"]) {
        warnings.push(payload["message"].clone());
    }
    let mut scopes: Vec<Value> = fixed_scopes(id)
        .into_iter()
        .map(|(value, label)| json!({"value":value,"label":label}))
        .collect();
    scopes.extend(
        rows.iter()
            .map(|r| json!({"value":format!("item:{}",string(r,"id")),"label":string(r,"label")})),
    );
    let attention = rows.iter().filter(|r| r["attention"] == true).count();
    let severity = rows
        .iter()
        .filter(|r| r["attention"] == true)
        .chain(warnings.iter());
    let attention_color = if severity.clone().any(|r| red(&r["color"])) {
        color::RED
    } else if severity.clone().any(|r| amber(&r["color"])) {
        color::AMBER
    } else {
        color::MUTED
    };
    let attention_label = if attention > 0 {
        format!("{title} · {attention}")
    } else {
        format!("{title} · check readings")
    };
    let hint = match id {
        "hosts" => "This machine and connected remote hosts",
        "ghWorkflows" => "Repositories watched by your GitHub accounts",
        "ghRunners" => "Self-hosted runners in watched GitHub organizations",
        "services" => "Connected providers and custom status pages",
        "sentryCrons" => "Scheduled jobs from your Sentry connection",
        "containers" => "Containers and virtual machines on monitored hosts",
        "claudeUsage" => "Local Claude Code logs and connected cloud providers",
        "azureCost" => "Cost exports from your Azure connection",
        "openclawAgents" => "Activity from your OpenClaw gateway",
        _ => "",
    };
    json!({"id":id,"title":title,"rows":rows,"message":message,"loading":payload["loading"]==true,"warnings":warnings,"scopes":scopes,"attentionCount":attention,"attentionColor":color::hex(attention_color),"attentionLabel":attention_label,"trailing":payload["trailing"],"hint":hint,
        "defaultTile":{"id":"draft","source":id,"title":title,"scope":if id=="sentryCrons" {"active"} else {"all"},"presentation":"summary","width":if id=="hosts" {"medium"} else {"small"},"hidden":false}})
}

fn in_scope(row: &Value, scope: &str) -> bool {
    scope == "all"
        || (scope == "attention" && row["attention"] == true)
        || scope
            .strip_prefix("item:")
            .is_some_and(|id| row["id"] == id)
        || list(row, "scopes").iter().any(|s| s == scope)
}

fn tile_view(tile: &DashboardTile, source: &Value) -> Value {
    let matching: Vec<_> = list(source, "rows")
        .iter()
        .filter(|r| in_scope(r, &tile.scope))
        .collect();
    let limit = if tile.presentation == "summary" {
        if tile.source == "hosts" {
            4
        } else {
            5
        }
    } else {
        12
    };
    // Resource ordering is stable across refreshes, including status changes.
    // Urgency lives in the fixed attention strip, never in moving tile targets.
    let shown: Vec<_> = matching.iter().take(limit).copied().cloned().collect();
    let hidden = matching.len().saturating_sub(limit);
    let hidden_attention = matching
        .iter()
        .skip(limit)
        .filter(|r| r["attention"] == true)
        .count();
    let problems = matching.iter().filter(|r| r["attention"] == true).count();
    let more = if hidden_attention > 0 {
        format!("{hidden} more · {hidden_attention} need attention →")
    } else {
        format!("{hidden} more →")
    };
    let scope_label = list(source, "scopes")
        .iter()
        .find(|s| s["value"] == tile.scope)
        .map(|s| string(s, "label").to_owned())
        .unwrap_or_else(|| "Resource no longer available".into());
    let (empty, empty_action) = if source["loading"] == true && list(source, "rows").is_empty() {
        ("Waiting for the first reading…", None)
    } else if tile.scope.starts_with("item:") && matching.is_empty() {
        (
            "This resource has no current reading. Review its connection or choose another scope.",
            Some("configure"),
        )
    } else if tile.scope != "all" && !list(source, "rows").is_empty() {
        (
            if ["attention", "active"].contains(&tile.scope.as_str())
                && list(source, "warnings").is_empty()
            {
                "No attention items in available readings."
            } else {
                "No resources match this scope."
            },
            Some("configure"),
        )
    } else if string(source, "message").is_empty() {
        ("No readings are available yet.", Some("manage"))
    } else {
        (string(source, "message"), Some("manage"))
    };
    json!({"id":tile.id,"source":tile.source,"title":tile.title,"width":tile.width,"presentation":tile.presentation,"scopeLabel":scope_label,"rows":shown,"empty":empty,"emptyAction":empty_action,"warnings":source["warnings"],"moreLabel":more,"moreCount":hidden,"footer":format!("{} shown · {problems} need attention",matching.len().min(limit))})
}

/// Same scope and truncation rules as a saved tile, using cached readings only.
pub fn preview(tile: &DashboardTile, snapshot: &Value) -> Result<Value, String> {
    validate(&DashboardLayout {
        revision: 0,
        tiles: vec![tile.clone()],
    })?;
    let source = list(snapshot, "sources")
        .iter()
        .find(|s| s["id"] == tile.source)
        .ok_or("That tile source is not available.")?;
    Ok(tile_view(tile, source))
}

pub fn view(layout: &DashboardLayout, payloads: &[Value]) -> Value {
    let sources: Vec<_> = SOURCES
        .iter()
        .map(|(id, title)| {
            let payload = payloads
                .iter()
                .find(|v| v["id"] == *id)
                .unwrap_or(&Value::Null);
            source_view(id, title, payload)
        })
        .collect();
    let (hidden_tiles, tiles): (Vec<_>, Vec<_>) = layout
        .tiles
        .iter()
        .filter_map(|tile| {
            sources
                .iter()
                .find(|s| s["id"] == tile.source)
                .map(|s| (tile.hidden, tile_view(tile, s)))
        })
        .partition(|(hidden, _)| *hidden);
    let tiles: Vec<_> = tiles.into_iter().map(|(_, tile)| tile).collect();
    let hidden_tiles: Vec<_> = hidden_tiles.into_iter().map(|(_, tile)| tile).collect();
    let attention: Vec<_> = sources
        .iter()
        .filter(|s| {
            s["attentionCount"].as_u64().unwrap_or(0) > 0 || !list(s, "warnings").is_empty()
        })
        .map(|s| json!({"source":s["id"],"label":s["attentionLabel"],"color":s["attentionColor"]}))
        .collect();
    json!({"layout":layout,"tiles":tiles,"hiddenTiles":hidden_tiles,"presets":presets(),"sources":sources,"attention":attention,"labels":labels()})
}

fn presets() -> Value {
    json!([
        {
            "id":"remote-machines",
            "hint":"Keep remote hosts together, with CPU and memory at a glance.",
            "tile":DashboardTile { id:"draft".into(), source:"hosts".into(), title:"Remote machines".into(), scope:"remote".into(), presentation:"summary".into(), width:"medium".into(), hidden:false }
        },
        {
            "id":"repos-attention",
            "hint":"Focus on repositories with issues in their available readings.",
            "tile":DashboardTile { id:"draft".into(), source:"ghWorkflows".into(), title:"Repos needing attention".into(), scope:"attention".into(), presentation:"summary".into(), width:"medium".into(), hidden:false }
        },
        {
            "id":"linux-runners",
            "hint":"See Linux runner availability in one compact tile.",
            "tile":DashboardTile { id:"draft".into(), source:"ghRunners".into(), title:"Linux runners".into(), scope:"LINUX".into(), presentation:"summary".into(), width:"small".into(), hidden:false }
        }
    ])
}

fn labels() -> Value {
    let entries = [
        ("title", "Solador"),
        ("subtitle", "Overview"),
        ("settings", "Settings"),
        ("allPanels", "All detailed panels →"),
        ("edit", "Edit dashboard"),
        ("done", "Done"),
        ("add", "Add tile"),
        ("undo", "Undo"),
        ("attention", "Needs attention"),
        ("attentionNote", "All sources, even without a tile"),
        ("quiet", "No attention items in available readings."),
        (
            "editHint",
            "Drag tiles or use the arrows. Hiding a tile keeps its source monitored.",
        ),
        ("empty", "No tiles in this view. Add a tile or restore a hidden one."),
        ("details", "Details →"),
        ("configure", "Configure"),
        ("hide", "Hide"),
        ("earlier", "Move earlier"),
        ("later", "Move later"),
        ("move", "Move"),
        ("close", "Close"),
        ("apply", "Apply"),
        ("duplicate", "Duplicate tile"),
        ("name", "Tile name"),
        ("scope", "Show"),
        ("presentation", "Presentation"),
        ("width", "Width"),
        ("position", "Position"),
        ("positionCurrent", "Keep current position"),
        ("positionStart", "At the beginning"),
        ("positionEnd", "At the end"),
        ("positionAfter", "After {title}"),
        ("placementPreview", "Dashboard placement"),
        (
            "placementHint",
            "Preview the order and widths. Your tile is highlighted.",
        ),
        ("positionMissing", "Position no longer available"),
        (
            "positionUnavailable",
            "That tile is no longer visible. Choose another position.",
        ),
        ("summary", "Summary"),
        ("detailed", "Detailed"),
        ("small", "Small"),
        ("medium", "Medium"),
        ("wide", "Wide"),
        ("catalog", "Add a tile"),
        ("presets", "Start with a preset"),
        ("presetsHint", "Customize its name, scope and placement before adding."),
        ("allSources", "All sources"),
        (
            "catalogHint",
            "Choose what to monitor, then set this tile's scope and size.",
        ),
        ("restore", "Restore"),
        ("restored", "Tile restored in its saved position."),
        ("hiddenTiles", "Hidden tiles"),
        ("hiddenCount", "Hidden tiles · {count}"),
        ("hiddenHint", "Saved here with their scope, size and position. Select a tile to preview its current readings."),
        ("hiddenEmpty", "No hidden tiles. Tiles you hide will be saved here to restore later."),
        ("hideSaved", "Tile hidden. Restore it from Hidden tiles, or Undo."),
        ("remove", "Remove tile"),
        ("removeNote", "Removing a tile keeps its connection and monitoring. Undo brings the tile back."),
        ("removed", "Tile removed. Its connection is still monitored. Undo restores the tile."),
        ("source", "Source"),
        ("fullPanel", "Open full panel"),
        ("back", "Overview"),
        ("manage", "Manage connection"),
        ("editScope", "Choose scope"),
        ("tilePreview", "Tile preview"),
        (
            "previewHint",
            "Current readings · saves in your chosen position",
        ),
        ("previewLoading", "Updating preview…"),
        (
            "previewFailed",
            "Preview unavailable. Your draft is still here.",
        ),
        ("openRepo", "Open on GitHub"),
        ("saved", "Dashboard saved."),
        (
            "failed",
            "Could not save the dashboard. Your previous layout is still active.",
        ),
        ("loading", "Loading overview…"),
        (
            "loadFailed",
            "Could not refresh the overview. Displayed readings may be out of date.",
        ),
        ("retry", "Retry"),
        ("hidden", "hidden"),
        ("preview", "Sample data · changes are not saved"),
        (
            "noSave",
            "This preview cannot save changes. Open Solador to edit your dashboard.",
        ),
        ("duplicateSuffix", " copy"),
        ("missingScope", "Resource no longer available"),
        (
            "missingReading",
            "No current reading is available for this resource.",
        ),
        (
            "scopeNote",
            "This tile has its own scope; other tiles and connections are unchanged.",
        ),
    ];
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), json!(value)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::{LayoutProfile, LayoutSlot, Store};

    #[test]
    fn disconnected_hosts_keep_unknown_metric_slots() {
        let rows = host_rows(&json!({"hosts": [{
            "id": "remote", "connection": {"state":"unreachable"},
            "error": {"hostName":"remote", "message":"Connection failed"},
            "cpuValue":"99%", "memValue":"31 GB"
        }]}));
        assert_eq!(
            rows[0]["metrics"],
            json!([field("CPU", &Value::Null), field("RAM", &Value::Null)])
        );
        assert_eq!(rows[0]["details"].as_array().unwrap().len(), 6);
        assert!(rows[0]["details"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["value"].is_null()));
    }

    #[test]
    fn filtered_empty_tiles_do_not_reuse_an_unrelated_source_message() {
        let source = source_view(
            "ghWorkflows",
            "GitHub repos",
            &json!({"message":{"text":"Source-wide message"},"rows":[
                {"repo":"acme/ok","name":"ok","status":"healthy","attention":false}
            ]}),
        );
        let mut tile = default_layout().tiles.remove(1);
        tile.scope = "attention".into();
        let empty = tile_view(&tile, &source);
        assert_eq!(empty["empty"], "No attention items in available readings.");
        assert_eq!(empty["emptyAction"], "configure");
        tile.scope = "item:acme/removed".into();
        let missing = tile_view(&tile, &source);
        assert!(list(&missing, "rows").is_empty());
        assert!(string(&missing, "empty").contains("no current reading"));
        let pending = source_view("ghWorkflows", "GitHub repos", &json!({"loading":true}));
        assert!(tile_view(&tile, &pending)["emptyAction"].is_null());
    }

    #[test]
    fn preview_uses_saved_tile_rules_without_mutating_the_dashboard() {
        let snapshot = crate::dump_dashboard();
        let before = snapshot.clone();
        let mut tile = default_layout().tiles.remove(0);
        tile.scope = "remote".into();
        tile.width = "wide".into();
        let preview = preview(&tile, &snapshot).unwrap();
        assert_eq!(preview["width"], "wide");
        assert_eq!(list(&preview, "rows").len(), 3);
        assert!(list(&preview, "rows").iter().all(|r| r["id"] != "local"));
        assert_eq!(snapshot, before);
        tile.scope = "invalid".into();
        assert!(super::preview(&tile, &snapshot).is_err());
    }

    #[test]
    fn hiding_duplicating_or_removing_tiles_cannot_change_attention() {
        let baseline = crate::dump_dashboard();
        let payload = crate::dump_crons(crate::crons::Fixture::Alerting, false);
        let mut layout = default_layout();
        let attention = view(&layout, std::slice::from_ref(&payload))["attention"].clone();
        layout.tiles.iter_mut().for_each(|t| t.hidden = true);
        assert!(list(&view(&layout, std::slice::from_ref(&payload)), "tiles").is_empty());
        assert_eq!(
            view(&layout, std::slice::from_ref(&payload))["attention"],
            attention
        );
        let mut duplicate = layout.tiles[4].clone();
        duplicate.id = "another-cron-view".into();
        duplicate.hidden = false;
        layout.tiles.push(duplicate);
        assert_eq!(
            view(&layout, std::slice::from_ref(&payload))["attention"],
            attention
        );
        layout.tiles.clear();
        let removed = view(&layout, &[payload]);
        assert_eq!(removed["attention"], attention);
        assert!(list(&removed, "tiles").is_empty());
        assert!(list(&removed, "hiddenTiles").is_empty());
        assert!(list(&baseline, "sources")
            .iter()
            .any(|s| s["id"] == "azureCost"));
    }

    #[test]
    fn hidden_library_preserves_the_same_scoped_readings_and_saved_slot() {
        let mut layout = default_layout();
        layout.tiles[4].scope = "attention".into();
        layout.tiles[4].width = "wide".into();
        let payload = crate::dump_crons(crate::crons::Fixture::Alerting, true);
        let visible = view(&layout, std::slice::from_ref(&payload));
        layout.tiles[4].hidden = true;
        let hidden = view(&layout, &[payload]);
        assert_eq!(hidden["hiddenTiles"][0], visible["tiles"][4]);
        assert_eq!(list(&hidden, "tiles").len(), 4);
        assert_eq!(hidden["layout"]["tiles"][4]["id"], "overview-sentryCrons");
        assert!(!list(&hidden["hiddenTiles"][0], "warnings").is_empty());
    }

    #[test]
    fn presets_are_valid_scoped_drafts_and_do_not_create_measured_states() {
        let snapshot = crate::dump_dashboard();
        let unmeasured = view(&default_layout(), &[]);
        let mut ids = BTreeSet::new();
        for preset in presets().as_array().unwrap() {
            assert!(ids.insert(preset["id"].as_str().unwrap().to_owned()));
            let tile: DashboardTile = serde_json::from_value(preset["tile"].clone()).unwrap();
            assert!(!tile.hidden);
            assert_ne!(tile.scope, "all");
            let projected = preview(&tile, &snapshot).unwrap();
            assert!(!list(&projected, "rows").is_empty());
            for row in list(&projected, "rows") {
                assert!(in_scope(row, &tile.scope));
            }
            let empty = preview(&tile, &unmeasured).unwrap();
            assert!(list(&empty, "rows").is_empty());
            assert_ne!(empty["empty"], "No attention items in available readings.");
        }
        assert_eq!(snapshot["layout"], json!(default_layout()));
    }

    #[test]
    fn scopes_are_independent_and_missing_resources_never_broaden() {
        let snapshot = crate::dump_dashboard();
        let source = list(&snapshot, "sources")
            .iter()
            .find(|s| s["id"] == "hosts")
            .unwrap();
        let mut t = default_layout().tiles.remove(0);
        t.scope = "local".into();
        let local = tile_view(&t, source);
        assert_eq!(list(&local, "rows").len(), 1);
        assert_eq!(local["rows"][0]["id"], "local");
        t.scope = "remote".into();
        assert_eq!(list(&tile_view(&t, source), "rows").len(), 3);
        t.scope = "item:retired-machine".into();
        assert!(list(&tile_view(&t, source), "rows").is_empty());
        assert_eq!(
            tile_view(&t, source)["scopeLabel"],
            "Resource no longer available"
        );
    }

    #[test]
    fn approximate_ages_suppression_and_stale_clocks_survive_summary() {
        let payload = crate::dump_crons(crate::crons::Fixture::Alerting, true);
        let vm = view(&default_layout(), &[payload]);
        let s = list(&vm, "sources")
            .iter()
            .find(|s| s["id"] == "sentryCrons")
            .unwrap();
        assert_eq!(s["attentionCount"], 3);
        assert_eq!(list(s, "warnings").len(), 2);
        let approximate = list(s, "rows")
            .iter()
            .find(|r| r["label"] == "nightly-rollup")
            .unwrap();
        assert_eq!(approximate["value"], "≈ 0d 22h");
        assert_eq!(approximate["valueColor"], color::hex(color::AMBER));
        let suppressed = list(s, "rows")
            .iter()
            .find(|r| r["label"] == "legacy-sweeper")
            .unwrap();
        assert_eq!(suppressed["attention"], false);
        let tile = list(&vm, "tiles")
            .iter()
            .find(|t| t["source"] == "sentryCrons")
            .unwrap();
        assert_eq!(list(tile, "rows").len(), 3);
    }

    #[test]
    fn setup_is_not_failure_and_failed_or_blind_reads_are_not_healthy() {
        for kind in [crate::crons::Fixture::Failed, crate::crons::Fixture::Blind] {
            let s = source_view(
                "sentryCrons",
                "Scheduled jobs",
                &crate::dump_crons(kind, false),
            );
            assert!(!list(&s, "warnings").is_empty());
        }
        let s = source_view(
            "sentryCrons",
            "Scheduled jobs",
            &crate::dump_crons(crate::crons::Fixture::Unconfigured, false),
        );
        assert!(list(&s, "warnings").is_empty());
        assert!(!string(&s, "message").is_empty());
        let p = json!({"hosts":[{"id":"pending","error":{"hostName":"new host","message":"waiting for first sample…"},"connection":{"state":"connecting","color":color::hex(color::AMBER)}}]});
        let row = &host_rows(&p)[0];
        assert_eq!(row["attention"], false);
        assert_eq!(row["value"], "Connecting");
        assert_eq!(list(row, "metrics").len(), 2);
        assert!(list(row, "metrics").iter().all(|m| m["value"].is_null()));
    }

    #[test]
    fn unknown_and_approval_need_attention_but_running_does_not() {
        let p = crate::dump_github(false, false);
        let rows = source_rows("ghWorkflows", &p);
        let get = |name| rows.iter().find(|r| r["label"] == name).unwrap();
        assert_eq!(get("toolkit")["attention"], true);
        assert_eq!(get("flywheel")["attention"], true);
        assert_eq!(get("pipe-fitting")["attention"], false);
        let services = crate::services::view(&crate::services::fixture_statuses());
        let rows = source_rows("services", &services);
        assert_eq!(
            rows.iter().find(|r| r["label"] == "Neon").unwrap()["attention"],
            true
        );
    }

    /// Every presentation paints `counts` on the row itself, so the three
    /// backlog numbers ride there — verbatim from the detailed table's cells,
    /// em dash included — and never in `metrics`, whose stat grid would put a
    /// second line under five rows and push the overview off a laptop.
    /// `details` is what is left, so Detailed does not print the three twice.
    #[test]
    fn repo_rows_carry_issues_ready_and_prs_on_the_row() {
        let p = crate::dump_github(false, false);
        let rows = source_rows("ghWorkflows", &p);
        let get = |name| rows.iter().find(|r| r["label"] == name).unwrap();
        let labels = |row: &Value, key: &str| -> Vec<String> {
            list(row, key)
                .iter()
                .map(|m| string(m, "label").to_owned())
                .collect()
        };
        let values = |row: &Value| -> Vec<String> {
            list(row, "counts")
                .iter()
                .map(|m| m["value"].as_str().unwrap_or("?").to_owned())
                .collect()
        };
        for row in &rows {
            assert_eq!(
                list(row, "counts")
                    .iter()
                    .map(|c| string(c, "header").to_owned())
                    .collect::<Vec<_>>(),
                ["ISSUES", "READY", "PRS"]
            );
            assert!(
                list(row, "metrics").is_empty(),
                "{}: a repo row stays one line",
                row["label"]
            );
            assert!(
                !labels(row, "details")
                    .iter()
                    .any(|l| ["ISSUES", "READY", "PRS"].contains(&l.as_str())),
                "{}: details repeat a metric",
                row["label"]
            );
        }
        // pipe-fitting: 9 inclusive − 2 PRs, 3 Ready, 2 PRs.
        assert_eq!(values(get("pipe-fitting")), ["7", "3", "2"]);
        assert_eq!(
            labels(get("pipe-fitting"), "counts"),
            ["issues", "ready", "PRs"]
        );
        // A genuine zero is "0", not an em dash — and plural, like the dash.
        assert_eq!(values(get("widget")), ["4", "0", "0"]);
        assert_eq!(labels(get("widget"), "counts"), ["issues", "ready", "PRs"]);
        // Exactly one is singular; `ready` has no plural.
        assert_eq!(values(get("flywheel")), ["1", "0", "1"]);
        assert_eq!(labels(get("flywheel"), "counts"), ["issue", "ready", "PR"]);
        // cogwheel's PAT could not read any side count; toolkit's runs failed.
        assert_eq!(values(get("cogwheel")), ["—", "—", "—"]);
        assert_eq!(
            labels(get("cogwheel"), "counts"),
            ["issues", "ready", "PRs"]
        );
        assert_eq!(values(get("toolkit")), ["—", "—", "—"]);
        // The detail line still names the longest run, found by column label
        // rather than a cell index that moves when a column joins.
        assert!(
            get("pipe-fitting")["detail"]
                .as_str()
                .unwrap()
                .contains("longest running: 1h35m"),
            "{}",
            get("pipe-fitting")["detail"]
        );
        assert_eq!(
            labels(get("widget"), "details"),
            ["REMOTE", "LOCAL", "WT", "JOBS", "LONGEST"]
        );
    }

    /// The CSS slots are hand-copied numbers; this is what ties them to the
    /// words. A word longer than its slot overflows into the separator on
    /// every row alike, which the alignment test cannot see.
    #[test]
    fn every_strip_word_and_status_fits_the_slot_the_css_reserves() {
        for (column, one, many) in ROW_COUNTS {
            let (_, slot) = ROW_COUNT_SLOT_CH
                .iter()
                .find(|(c, _)| *c == column)
                .expect("every column has a slot");
            assert!(
                one.len() <= *slot && many.len() <= *slot,
                "{column}: {one}/{many} in {slot}ch"
            );
        }
        let p = crate::dump_github(false, false);
        for row in list(&p, "rows") {
            let status = string(row, "statusLabel");
            assert!(
                status.len() <= ROW_STATUS_SLOT_CH,
                "{status} in {ROW_STATUS_SLOT_CH}ch"
            );
        }
        assert!(
            list(&p, "rows")
                .iter()
                .any(|r| r["statusLabel"] == "Needs approval"),
            "the widest status is in the fixture, so the bound is exercised"
        );
    }

    /// The mixed row: READY alone refused beside counts that came back — the
    /// shape a PAT without the Projects permission produces on every repo.
    /// The dash is the cell's own, carried verbatim, and plural like a zero.
    #[test]
    fn a_refused_ready_beside_known_counts_keeps_the_cells_own_dash() {
        let mut p = crate::dump_github(false, false);
        let ready = list(&p, "columns")
            .iter()
            .position(|c| c["label"] == crate::github::COL_READY)
            .expect("READY column")
            - 1; // cells omit the REPO column
        for row in p["rows"].as_array_mut().expect("rows") {
            if row["name"] == "pipe-fitting" {
                row["cells"][ready]["text"] = json!("—");
            }
        }
        let rows = source_rows("ghWorkflows", &p);
        let row = rows.iter().find(|r| r["label"] == "pipe-fitting").unwrap();
        let strip: Vec<String> = list(row, "counts")
            .iter()
            .map(|c| format!("{} {}", c["value"].as_str().unwrap(), string(c, "label")))
            .collect();
        assert_eq!(strip, ["7 issues", "— ready", "2 PRs"]);
    }

    #[test]
    fn warnings_keep_severity_and_runtime_failures_survive_the_overview() {
        let warning = json!({"budget":{"label":"Budget","value":"85%","bar":{"color":color::hex(color::AMBER)}}});
        let source = source_view("azureCost", "Azure Cost", &warning);
        assert_eq!(source["attentionCount"], 1);
        assert_eq!(source["attentionColor"], color::hex(color::AMBER));
        for (fixture, severity) in [
            (crate::openclaw::Fixture::Disconnected, color::RED),
            (crate::openclaw::Fixture::Pairing, color::AMBER),
        ] {
            let source = source_view("openclawAgents", "OpenClaw", &crate::dump_openclaw(fixture));
            assert!(source["attentionCount"].as_u64().unwrap() > 0);
            assert_eq!(source["attentionColor"], color::hex(severity));
        }
    }

    #[test]
    fn resource_scopes_follow_identity_when_openclaw_rows_move() {
        let mut payload = crate::dump_openclaw(crate::openclaw::Fixture::Connected);
        let before = source_rows("openclawAgents", &payload);
        let agents = payload["runtimes"][0]["agents"]["rows"]
            .as_array_mut()
            .unwrap();
        assert!(agents.len() > 1);
        agents.reverse();
        let after = source_rows("openclawAgents", &payload);
        for original in before {
            let moved = after
                .iter()
                .find(|row| row["id"] == original["id"])
                .unwrap();
            assert_eq!(moved["label"], original["label"]);
        }
    }

    #[test]
    fn truncation_names_problems_outside_the_visible_rows() {
        let mut source = source_view("services", "Services", &json!({}));
        source["rows"] = json!((0..8)
            .map(|i| row(
                &i.to_string(),
                "Vendor",
                "Status",
                json!(color::hex(color::RED)),
                i >= 5
            ))
            .collect::<Vec<_>>());
        let t = default_layout().tiles.remove(3);
        let tile = tile_view(&t, &source);
        assert_eq!(tile["moreCount"], 3);
        assert_eq!(tile["moreLabel"], "3 more · 3 need attention →");
        assert_eq!(list(&tile, "rows").len(), 5);
    }

    #[test]
    fn save_round_trips_without_changing_connections_or_detailed_layout() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_in(dir.path(), false).unwrap();
        store.upsert_host(store::Host::new("remote", "10.0.0.2"));
        store.set_layout(vec![LayoutProfile::new(
            1200.0,
            "tabs",
            vec![LayoutSlot::new("hosts", "half")],
        )]);
        let before = store.data().clone();
        let mut layout = default_layout();
        layout.tiles.reverse();
        layout.tiles[0].hidden = true;
        let saved = crate::persist_dashboard(&mut store, layout, 0).unwrap();
        assert_eq!(saved.revision, 1);
        let reopened = Store::open_in(dir.path(), false).unwrap();
        assert_eq!(reopened.dashboard(), Some(&saved));
        assert_eq!(reopened.data().hosts, before.hosts);
        assert_eq!(reopened.data().settings, before.settings);
        assert_eq!(reopened.data().layout, before.layout);
        let conflict = crate::persist_dashboard(&mut store, default_layout(), 0).unwrap_err();
        assert!(conflict.contains("changed"));
        assert_eq!(store.dashboard(), Some(&saved));
        let empty = DashboardLayout {
            revision: 0,
            tiles: vec![],
        };
        crate::persist_dashboard(&mut store, empty, 1).unwrap();
        assert!(Store::open_in(dir.path(), false)
            .unwrap()
            .dashboard()
            .unwrap()
            .tiles
            .is_empty());
    }

    #[test]
    fn failed_write_rolls_back_and_invalid_edits_never_reach_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_in(dir.path(), false).unwrap();
        let saved = crate::persist_dashboard(&mut store, default_layout(), 0).unwrap();
        let original = std::fs::read(store.path()).unwrap();
        let mut invalid = saved.clone();
        invalid.tiles[0].scope = "LINUX".into();
        assert!(crate::persist_dashboard(&mut store, invalid, 1).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), original);
        std::fs::remove_file(store.path()).unwrap();
        std::fs::create_dir(store.path()).unwrap();
        assert!(crate::persist_dashboard(&mut store, default_layout(), 1).is_err());
        assert_eq!(store.dashboard(), Some(&saved));
    }
}
