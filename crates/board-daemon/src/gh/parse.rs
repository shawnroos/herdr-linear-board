//! `gh` JSON to the protocol's PR detail. Pure: no process, no clock.

use std::collections::BTreeMap;

use board_core::protocol::{LinearPrCheck, LinearPrCi, LinearPrDetail, LinearPrReview};
use serde_json::Value;

pub const VIEW_FIELDS: &str = "number,title,url,state,isDraft,mergedAt,mergeStateStatus,mergeable,\
reviewDecision,additions,deletions,changedFiles,headRefName,headRefOid,statusCheckRollup,latestReviews";

pub const CHECKS_FIELDS: &str = "name,state,bucket";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredCheck {
    pub name: String,
    pub state: String,
    pub bucket: String,
}

pub(crate) fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// `None` when the output is not the JSON array `gh` prints, which is how a
/// PR with no required checks answers.
pub fn required_checks(stdout: &[u8]) -> Option<Vec<RequiredCheck>> {
    let rows: Vec<Value> = serde_json::from_slice(stdout).ok()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                Some(RequiredCheck {
                    name: text(&row["name"])?,
                    state: row["state"].as_str().unwrap_or("").to_string(),
                    bucket: row["bucket"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect(),
    )
}

fn bucket(item: &Value) -> (&'static str, Option<String>) {
    if let Some(state) = item["state"]
        .as_str()
        .filter(|_| item["context"].is_string())
    {
        let bucket = match state {
            "SUCCESS" => "pass",
            "PENDING" | "EXPECTED" => "pending",
            _ => "fail",
        };
        return (bucket, Some(state.to_string()));
    }
    let status = item["status"].as_str().unwrap_or("");
    if status != "COMPLETED" {
        return ("pending", text(&item["status"]));
    }
    let conclusion = item["conclusion"].as_str().unwrap_or("");
    let bucket = match conclusion {
        "SUCCESS" | "NEUTRAL" => "pass",
        "SKIPPED" => "skipping",
        _ => "fail",
    };
    (bucket, text(&item["conclusion"]))
}

fn latest_checks(rollup: &Value, required: &[RequiredCheck]) -> Vec<LinearPrCheck> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: BTreeMap<String, (String, LinearPrCheck)> = BTreeMap::new();
    for item in rollup.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(name) = text(&item["name"]).or_else(|| text(&item["context"])) else {
            continue;
        };
        let at = [&item["completedAt"], &item["startedAt"]]
            .iter()
            .filter_map(|v| v.as_str())
            .max()
            .unwrap_or("")
            .to_string();
        let (bucket, state) = bucket(item);
        let check = LinearPrCheck {
            required: required.iter().any(|r| r.name == name),
            name: name.clone(),
            bucket: bucket.to_string(),
            state,
            url: text(&item["detailsUrl"]).or_else(|| text(&item["targetUrl"])),
        };
        match latest.get(&name) {
            Some((seen, _)) if *seen > at => {}
            Some(_) => {
                latest.insert(name, (at, check));
            }
            None => {
                order.push(name.clone());
                latest.insert(name, (at, check));
            }
        }
    }
    order
        .iter()
        .filter_map(|name| latest.remove(name).map(|(_, check)| check))
        .collect()
}

fn count(checks: &[LinearPrCheck]) -> LinearPrCi {
    let n = |b: &str| checks.iter().filter(|c| c.bucket == b).count() as u64;
    let mut ci = LinearPrCi {
        state: String::new(),
        passed: n("pass"),
        pending: n("pending"),
        failed: n("fail"),
        skipped: n("skipping"),
    };
    ci.state = if ci.failed > 0 {
        "fail"
    } else if ci.pending > 0 {
        "pending"
    } else if ci.passed > 0 {
        "pass"
    } else {
        "none"
    }
    .to_string();
    ci
}

/// What keeps an open PR from merging: the first required check that is not
/// passing, else the review decision, when GitHub reports it blocked.
fn blocker(
    view: &Value,
    checks: &[LinearPrCheck],
    required: Option<&[RequiredCheck]>,
) -> Option<String> {
    if view["state"].as_str() != Some("OPEN") {
        return None;
    }
    match view["mergeStateStatus"].as_str()? {
        "DIRTY" => return Some("has merge conflicts".into()),
        "BLOCKED" => {}
        _ => return None,
    }
    let failing = |bucket: &str| match bucket {
        "pending" => Some("pending"),
        "fail" | "cancel" => Some("failing"),
        _ => None,
    };
    let from_required = match required {
        Some(rows) => rows
            .iter()
            .find_map(|r| failing(&r.bucket).map(|word| (r.name.clone(), word))),
        None => checks
            .iter()
            .filter(|c| c.required)
            .find_map(|c| failing(&c.bucket).map(|word| (c.name.clone(), word))),
    };
    if let Some((name, word)) = from_required {
        return Some(format!("{name} (required) is {word}"));
    }
    Some(
        match view["reviewDecision"].as_str() {
            Some("CHANGES_REQUESTED") => "changes requested",
            Some("REVIEW_REQUIRED") => "review required",
            _ => "merge blocked",
        }
        .into(),
    )
}

/// The caller fills in `repo` and the preview, which need more `gh` calls.
pub fn pr_detail(view: &Value, required: Option<&[RequiredCheck]>) -> LinearPrDetail {
    let checks = latest_checks(&view["statusCheckRollup"], required.unwrap_or(&[]));
    LinearPrDetail {
        repo: String::new(),
        number: view["number"].as_u64().unwrap_or(0),
        url: view["url"].as_str().unwrap_or("").to_string(),
        title: text(&view["title"]),
        state: text(&view["state"]),
        draft: view["isDraft"].as_bool(),
        merged_at: text(&view["mergedAt"]),
        merge_state: text(&view["mergeStateStatus"]),
        mergeable: text(&view["mergeable"]),
        review_decision: text(&view["reviewDecision"]),
        additions: view["additions"].as_u64(),
        deletions: view["deletions"].as_u64(),
        changed_files: view["changedFiles"].as_u64(),
        head_ref: text(&view["headRefName"]),
        head_sha: text(&view["headRefOid"]),
        ci: Some(count(&checks)),
        blocker: blocker(view, &checks, required),
        reviews: view["latestReviews"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|r| LinearPrReview {
                author: text(&r["author"]["login"]),
                state: text(&r["state"]),
                submitted_at: text(&r["submittedAt"]),
            })
            .collect(),
        checks,
        preview: None,
    }
}

pub fn newest_deployment(stdout: &[u8]) -> Option<u64> {
    let rows: Vec<Value> = serde_json::from_slice(stdout).ok()?;
    rows.iter()
        .filter_map(|d| Some((d["created_at"].as_str().unwrap_or(""), d["id"].as_u64()?)))
        .max_by(|a, b| a.0.cmp(b.0))
        .map(|(_, id)| id)
}

pub fn preview_url(stdout: &[u8]) -> Option<String> {
    let rows: Vec<Value> = serde_json::from_slice(stdout).ok()?;
    rows.iter()
        .filter_map(|s| {
            Some((
                s["created_at"].as_str().unwrap_or(""),
                text(&s["environment_url"])?,
            ))
        })
        .max_by(|a, b| a.0.cmp(b.0))
        .map(|(_, url)| url)
}
