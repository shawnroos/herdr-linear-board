use serde::Deserialize;
use serde_json::Value;

use crate::types::AgentStatus;

/// A decoded event. Tolerant: unknown event kinds become [`HerdrEvent::Other`]
/// carrying the raw line, and unknown fields are ignored.
#[derive(Debug, Clone, PartialEq)]
pub enum HerdrEvent {
    /// `pane_agent_status_changed`.
    AgentStatusChanged {
        pane_id: String,
        workspace_id: Option<String>,
        status: AgentStatus,
        agent: Option<String>,
    },
    /// `pane_exited` or `pane_closed` — the pane is gone.
    PaneExited {
        pane_id: String,
        workspace_id: Option<String>,
    },
    /// `pane_focused`, `tab_focused` or `layout_updated`. `pane_focused`
    /// carries no tab; `layout_updated` nests both ids in `layout`.
    LayoutChanged {
        workspace_id: Option<String>,
        tab_id: Option<String>,
    },
    /// `tab_created` or `pane_created`; the id is nested in `tab` or `pane`.
    Created { workspace_id: Option<String> },
    /// Any other event line (raw envelope preserved).
    Other(Value),
}

#[derive(Deserialize)]
struct StatusFields {
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    agent_status: Option<AgentStatus>,
    #[serde(default)]
    agent: Option<String>,
}

#[derive(Deserialize)]
struct PlaceFields {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Deserialize)]
struct PaneFields {
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

/// Parse one raw NDJSON line into an event, or `None` if it is not an event
/// (e.g. the `subscription_started` ack, an error/ack line, or blank).
pub fn parse_event_line(line: &str) -> Option<HerdrEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    // Skip request/response envelopes (acks, errors) — they carry an `id`.
    if value.get("result").is_some() || value.get("error").is_some() {
        return None;
    }
    // Event body: prefer the nested `data` object, else the value itself.
    let data = match value.get("data") {
        Some(d) if d.is_object() => d,
        _ => &value,
    };
    // The kind lives in `data.type` (underscore names) on some herdr builds and
    // in the top-level `event` key (dotted names) on others (verified live on
    // protocol 19: {"event":"pane.agent_status_changed","data":{...}} with
    // no data.type). Accept both, normalized to underscores.
    let kind = data
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.get("event").and_then(Value::as_str))?
        .replace('.', "_");
    match kind.as_str() {
        "pane_agent_status_changed" => {
            let f: StatusFields = serde_json::from_value(data.clone()).ok()?;
            let pane_id = f.pane_id?;
            Some(HerdrEvent::AgentStatusChanged {
                pane_id,
                workspace_id: f.workspace_id,
                status: f.agent_status.unwrap_or(AgentStatus::Unknown),
                agent: f.agent,
            })
        }
        "pane_exited" | "pane_closed" => {
            let f: PaneFields = serde_json::from_value(data.clone()).ok()?;
            let pane_id = f.pane_id?;
            Some(HerdrEvent::PaneExited {
                pane_id,
                workspace_id: f.workspace_id,
            })
        }
        "pane_focused" | "tab_focused" | "layout_updated" => {
            let place = data.get("layout").unwrap_or(data);
            let f: PlaceFields = serde_json::from_value(place.clone()).ok()?;
            Some(HerdrEvent::LayoutChanged {
                workspace_id: f.workspace_id,
                tab_id: f.tab_id,
            })
        }
        "tab_created" | "pane_created" => {
            let place = data.get("tab").or_else(|| data.get("pane")).unwrap_or(data);
            let f: PlaceFields = serde_json::from_value(place.clone()).ok()?;
            Some(HerdrEvent::Created {
                workspace_id: f.workspace_id,
            })
        }
        _ => Some(HerdrEvent::Other(value)),
    }
}
