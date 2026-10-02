//! Grouping-engine table tests. The plugin fixtures are ported from shrimpshack
//! `plugins/work/tests/fixtures/board/{config,tickets}.json`; the config is
//! rebuilt by hand because the board's store refuses the plugin's `version`,
//! `source` and `state-type-not` keys.

use std::collections::BTreeMap;

use board_core::db::{GroupingConfig, GroupingMapping, SpaceGrouping};
use board_core::engine::grouping::{
    choose, fill_snapshot, group_board, issue_filter, BoardGrouping, GroupingIssue,
    ViewPreferences, WorkflowState,
};
use board_core::protocol::{LinearGroup, LinearSnapshot, LinearTab};
use serde_json::{json, Value};

fn mapping(levels: &[(&str, &str)], filter: Value) -> GroupingMapping {
    GroupingMapping {
        levels: levels
            .iter()
            .map(|(l, k)| (l.to_string(), k.to_string()))
            .collect::<BTreeMap<_, _>>(),
        filter: filter.as_object().unwrap().clone(),
    }
}

fn issues(value: Value) -> Vec<GroupingIssue> {
    serde_json::from_value(value).unwrap()
}

fn plugin_tickets() -> Vec<GroupingIssue> {
    issues(json!([
      {"id": "iss-1", "identifier": "WEB-1", "title": "Login form", "updatedAt": "2026-09-10T10:00:00.000Z",
       "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
       "team": {"id": "team-web", "key": "WEB"}, "project": {"id": "prj-alpha", "name": "Alpha"},
       "projectMilestone": null, "cycle": null, "assignee": {"id": "user-ana", "name": "Ana"},
       "priority": 2, "parent": null, "labels": {"nodes": []}},
      {"id": "iss-2", "identifier": "WEB-2", "title": "Unowned bug", "updatedAt": "2026-09-10T10:00:00.000Z",
       "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
       "team": {"id": "team-web", "key": "WEB"}, "project": null,
       "projectMilestone": null, "cycle": null, "assignee": null,
       "priority": 0, "parent": null, "labels": {"nodes": []}},
      {"id": "iss-3", "identifier": "WEB-3", "title": "Search index", "updatedAt": "2026-09-10T10:00:00.000Z",
       "state": {"id": "st-prog", "name": "In Progress", "type": "started"},
       "team": {"id": "team-web", "key": "WEB"}, "project": {"id": "prj-alpha", "name": "Alpha"},
       "projectMilestone": null, "cycle": null, "assignee": {"id": "user-ana", "name": "Ana"},
       "priority": 1, "parent": null, "labels": {"nodes": []}},
      {"id": "iss-4", "identifier": "OPS-4", "title": "Rotate keys", "updatedAt": "2026-09-10T10:00:00.000Z",
       "state": {"id": "st-ops-todo", "name": "Todo", "type": "unstarted"},
       "team": {"id": "team-ops", "key": "OPS"}, "project": null,
       "projectMilestone": null, "cycle": null, "assignee": {"id": "user-ben", "name": "Ben"},
       "priority": 3, "parent": null, "labels": {"nodes": []}},
      {"id": "iss-6", "identifier": "WEB-6", "title": "Signup copy", "updatedAt": "2026-09-10T10:00:00.000Z",
       "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
       "team": {"id": "team-web", "key": "WEB"}, "project": null,
       "projectMilestone": null, "cycle": null, "assignee": {"id": "user-ana", "name": "Ana"},
       "priority": 4, "parent": null, "labels": {"nodes": []}}
    ]))
}

fn plugin_config() -> GroupingConfig {
    GroupingConfig {
        global: mapping(
            &[("space", "team"), ("tab", "assignee"), ("column", "state")],
            json!({"team": ["WEB", "OPS"]}),
        ),
        spaces: vec![
            SpaceGrouping {
                space: "Mine".into(),
                mapping: mapping(&[("tab", "priority")], json!({"assignee": "me"})),
            },
            SpaceGrouping {
                space: "Later".into(),
                mapping: mapping(&[("tab", "priority")], json!({"label": "later"})),
            },
        ],
    }
}

/// `[[tab, [[column, cards | [[lane, cards]]]]]]`, labels only, in order.
fn shape(tabs: &[LinearTab]) -> Value {
    Value::Array(
        tabs.iter()
            .map(|tab| json!([tab.label, columns(&tab.groups)]))
            .collect(),
    )
}

fn columns(groups: &[LinearGroup]) -> Value {
    Value::Array(
        groups
            .iter()
            .map(|g| {
                if g.lanes.is_empty() {
                    json!([g.label, g.issues])
                } else {
                    let lanes: Vec<Value> =
                        g.lanes.iter().map(|l| json!([l.label, l.issues])).collect();
                    json!([g.label, lanes])
                }
            })
            .collect(),
    )
}

fn states() -> Vec<WorkflowState> {
    serde_json::from_value(json!([
        {"id": "st-triage", "name": "Triage", "type": "triage"},
        {"id": "st-todo", "name": "Todo", "type": "unstarted"},
        {"id": "st-prog", "name": "In Progress", "type": "started"},
        {"id": "st-done", "name": "Done", "type": "completed"},
        {"id": "st-cancel", "name": "Canceled", "type": "canceled"}
    ]))
    .unwrap()
}

#[test]
fn the_plugin_fixture_groups_one_team_space_by_assignee_tab_and_state_column() {
    let config = plugin_config();
    let tickets = plugin_tickets();
    let web = group_board(&tickets, choose(Some(&config), "WEB", None, &[]));
    assert_eq!(
        shape(&web),
        json!([
            [
                "Ana",
                [["Todo", ["WEB-1", "WEB-6"]], ["In Progress", ["WEB-3"]]]
            ],
            ["No assignee", [["Todo", ["WEB-2"]]]]
        ])
    );
    assert_eq!(web[0].key, "user-ana");
    assert_eq!(web[1].key, "", "a No <field> group has an empty key");
    assert_eq!(web[0].groups[0].kind.as_deref(), Some("unstarted"));

    let ops = group_board(&tickets, choose(Some(&config), "OPS", None, &[]));
    assert_eq!(shape(&ops), json!([["Ben", [["Todo", ["OPS-4"]]]]]));
}

#[test]
fn tab_by_parent_columns_by_state_rows_by_assignee_nest_as_configured() {
    let tickets = issues(json!([
        {"id": "u1", "identifier": "WEB-11", "title": "a",
         "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
         "assignee": {"id": "user-ana", "name": "Ana"}, "parent": {"id": "p1", "identifier": "WEB-10"}},
        {"id": "u2", "identifier": "WEB-12", "title": "b",
         "state": {"id": "st-prog", "name": "In Progress", "type": "started"},
         "assignee": {"id": "user-ben", "name": "Ben"}, "parent": {"id": "p1", "identifier": "WEB-10"}},
        {"id": "u3", "identifier": "WEB-13", "title": "c",
         "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
         "assignee": {"id": "user-ben", "name": "Ben"}, "parent": {"id": "p1", "identifier": "WEB-10"}},
        {"id": "u4", "identifier": "WEB-21", "title": "d",
         "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
         "assignee": {"id": "user-ana", "name": "Ana"}, "parent": {"id": "p2", "identifier": "WEB-9"}},
        {"id": "u5", "identifier": "WEB-30", "title": "e",
         "state": {"id": "st-todo", "name": "Todo", "type": "unstarted"},
         "assignee": null, "parent": null}
    ]));
    let m = mapping(
        &[("tab", "parent"), ("column", "state"), ("row", "assignee")],
        json!({"team": "WEB"}),
    );
    let tabs = group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    );
    assert_eq!(
        shape(&tabs),
        json!([
            ["WEB-9", [["Todo", [["Ana", ["WEB-21"]]]]]],
            [
                "WEB-10",
                [
                    ["Todo", [["Ana", ["WEB-11"]], ["Ben", ["WEB-13"]]]],
                    ["In Progress", [["Ana", []], ["Ben", ["WEB-12"]]]]
                ]
            ],
            ["No parent", [["Todo", [["No assignee", ["WEB-30"]]]]]]
        ])
    );
}

#[test]
fn an_issue_with_no_assignee_lands_in_the_no_assignee_lane() {
    let tickets = plugin_tickets();
    let m = mapping(
        &[("column", "state"), ("row", "assignee")],
        json!({"team": "WEB"}),
    );
    let tabs = group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    );
    let todo = tabs[0].groups.iter().find(|g| g.key == "st-todo").unwrap();
    let lane = todo
        .lanes
        .iter()
        .find(|l| l.label == "No assignee")
        .unwrap();
    assert_eq!(lane.key, "");
    assert_eq!(lane.issues, vec!["WEB-2".to_string()]);
    assert_eq!(todo.lanes.last().unwrap().label, "No assignee");
}

#[test]
fn a_space_override_replaces_the_global_mapping_for_that_space_only() {
    let config = plugin_config();
    let tickets = plugin_tickets();
    let mine = group_board(&tickets, choose(Some(&config), "Mine", None, &[]));
    assert_eq!(
        shape(&mine),
        json!([
            ["Urgent", [["Issues", ["WEB-3"]]]],
            ["High", [["Issues", ["WEB-1"]]]],
            ["Medium", [["Issues", ["OPS-4"]]]],
            ["Low", [["Issues", ["WEB-6"]]]],
            ["No priority", [["Issues", ["WEB-2"]]]]
        ])
    );
    let web = group_board(&tickets, choose(Some(&config), "WEB", None, &[]));
    assert_eq!(web[0].label, "Ana", "the global mapping still governs WEB");
    assert!(matches!(
        choose(Some(&config), "Later", None, &[]),
        BoardGrouping::Configured { mapping, space: None } if mapping == &config.spaces[1].mapping
    ));
}

#[test]
fn column_ticket_leaves_columns_ungrouped() {
    let tickets = plugin_tickets();
    let m = mapping(&[("column", "ticket")], json!({"team": "WEB"}));
    let tabs = group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    );
    assert_eq!(
        shape(&tabs),
        json!([[
            "Board",
            [["Issues", ["OPS-4", "WEB-1", "WEB-2", "WEB-3", "WEB-6"]]]
        ]])
    );
}

#[test]
fn a_sub_ticket_level_keeps_cards_ungrouped_with_each_parent_before_its_children() {
    let tickets = issues(json!([
        {"id": "c2", "identifier": "WEB-12", "parent": {"id": "p", "identifier": "WEB-2"}},
        {"id": "x", "identifier": "WEB-3"},
        {"id": "p", "identifier": "WEB-2"},
        {"id": "c1", "identifier": "WEB-11", "parent": {"id": "p", "identifier": "WEB-2"}},
        {"id": "y", "identifier": "WEB-1"}
    ]));
    let m = mapping(&[("row", "sub-ticket")], json!({"team": "WEB"}));
    let tabs = group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    );
    assert_eq!(
        shape(&tabs),
        json!([[
            "Board",
            [["Issues", ["WEB-1", "WEB-2", "WEB-11", "WEB-12", "WEB-3"]]]
        ]])
    );
}

#[test]
fn with_no_config_a_custom_view_seeds_columns_and_lanes_unchanged() {
    let tickets = plugin_tickets();
    let view: ViewPreferences = serde_json::from_value(json!({
        "layout": "board",
        "issueGrouping": "workflowState",
        "issueSubGrouping": "assignee",
        "columnOrderBoard": ["st-prog", "st-todo", "st-done"],
        "hiddenColumns": ["st-done"],
        "hiddenRows": ["user-ben"]
    }))
    .unwrap();
    let st = states();
    let grouping = choose(None, "WEB", Some(&view), &st);
    assert!(matches!(grouping, BoardGrouping::View { .. }));
    let tabs = group_board(&tickets, grouping);
    assert_eq!(
        shape(&tabs),
        json!([[
            "Board",
            [
                ["In Progress", [["Ana", ["WEB-3"]], ["Unassigned", []]]],
                [
                    "Todo",
                    [["Ana", ["WEB-1", "WEB-6"]], ["Unassigned", ["WEB-2"]]]
                ],
                ["Todo", [["Ana", []], ["Unassigned", []]]]
            ]
        ]])
    );
    let todo = &tabs[0].groups[1];
    assert_eq!(todo.key, "st-todo");
    assert_eq!(todo.kind.as_deref(), Some("unstarted"));
    assert_eq!(todo.lanes[1].key, "unassigned");
}

#[test]
fn a_view_grouping_by_priority_keeps_the_scripts_keys_and_drops_state_ids_from_its_order() {
    let tickets = plugin_tickets();
    let view: ViewPreferences = serde_json::from_value(json!({
        "issueGrouping": "priority",
        "columnOrderBoard": ["st-todo", "3", "1"],
        "hiddenColumns": null
    }))
    .unwrap();
    let st = states();
    let tabs = group_board(&tickets, choose(None, "WEB", Some(&view), &st));
    let cols: Vec<(&str, &str, Option<&str>)> = tabs[0]
        .groups
        .iter()
        .map(|g| (g.key.as_str(), g.label.as_str(), g.kind.as_deref()))
        .collect();
    assert_eq!(
        cols,
        vec![
            ("3", "Medium", None),
            ("1", "Urgent", None),
            ("2", "High", None),
            ("0", "No priority", None),
            ("4", "Low", None),
        ]
    );
}

#[test]
fn with_no_config_and_no_view_the_board_shows_team_columns_without_canceled() {
    let tickets = plugin_tickets();
    let st = states();
    let grouping = choose(None, "WEB", None, &st);
    assert!(matches!(grouping, BoardGrouping::Team { .. }));
    let tabs = group_board(&tickets, grouping);
    assert_eq!(
        shape(&tabs),
        json!([[
            "Board",
            [
                ["Triage", []],
                ["Todo", ["WEB-1", "WEB-2", "WEB-6"]],
                ["In Progress", ["WEB-3"]],
                ["Done", []],
                ["Todo", ["OPS-4"]]
            ]
        ]])
    );
}

#[test]
fn an_unsupported_view_grouping_falls_back_to_every_team_state() {
    let tickets = plugin_tickets();
    let view: ViewPreferences =
        serde_json::from_value(json!({"issueGrouping": "cycle", "issueSubGrouping": "cycle"}))
            .unwrap();
    let st = states();
    let tabs = group_board(&tickets, choose(None, "WEB", Some(&view), &st));
    let labels: Vec<&str> = tabs[0].groups.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["Triage", "Todo", "In Progress", "Done", "Todo"]
    );
    assert!(tabs[0].groups.iter().all(|g| g.lanes.is_empty()));
}

#[test]
fn output_is_byte_stable_for_the_same_input_in_any_order() {
    let config = plugin_config();
    let mut tickets = plugin_tickets();
    let m = mapping(
        &[("tab", "project"), ("column", "state"), ("row", "priority")],
        json!({"team": "WEB"}),
    );
    let first = serde_json::to_string(&group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    ))
    .unwrap();
    tickets.reverse();
    tickets.swap(0, 2);
    let second = serde_json::to_string(&group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    ))
    .unwrap();
    assert_eq!(first, second);
    assert!(first.contains("\"WEB-1\"") && first.contains("\"Alpha\""));
    let a = serde_json::to_string(&group_board(
        &tickets,
        choose(Some(&config), "WEB", None, &[]),
    ))
    .unwrap();
    let b = serde_json::to_string(&group_board(
        &tickets,
        choose(Some(&config), "WEB", None, &[]),
    ))
    .unwrap();
    assert_eq!(a, b);
}

#[test]
fn an_explicit_null_in_an_incoming_issue_field_reads_as_empty() {
    let parsed: Result<Vec<GroupingIssue>, _> = serde_json::from_value(json!([{
        "id": null, "identifier": "WEB-5", "title": null, "updatedAt": null, "url": null,
        "state": {"id": null, "name": null, "type": null},
        "team": {"id": null, "key": null, "name": null},
        "project": {"id": null, "name": null},
        "projectMilestone": null, "cycle": {"id": null, "number": null, "name": null},
        "assignee": {"id": null, "name": null}, "priority": null,
        "parent": {"id": null, "identifier": null},
        "labels": {"nodes": null}
    }, {"identifier": "WEB-6", "labels": null}]));
    let tickets = parsed.expect("explicit nulls parse");
    let m = mapping(
        &[("column", "assignee"), ("row", "state")],
        json!({"team": "WEB"}),
    );
    let tabs = group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    );
    assert_eq!(
        shape(&tabs),
        json!([[
            "Board",
            [["No assignee", [["No state", ["WEB-5", "WEB-6"]]]]]
        ]])
    );

    let view: ViewPreferences = serde_json::from_value(json!({
        "layout": null, "issueGrouping": null, "issueSubGrouping": null,
        "hiddenColumns": null, "hiddenRows": null, "columnOrderBoard": null
    }))
    .expect("a view with explicit nulls parses");
    assert_eq!(view, ViewPreferences::default());
    let st: Vec<WorkflowState> =
        serde_json::from_value(json!([{"id": "s", "name": null, "type": null}])).unwrap();
    assert_eq!(st[0].name, "");
}

#[test]
fn label_groups_and_cycles_and_milestones_group_by_their_linear_ids() {
    let tickets = issues(json!([
        {"identifier": "WEB-1", "cycle": {"id": "cy-2", "number": 2},
         "labels": {"nodes": [{"id": "l-b", "name": "Backend", "parent": {"id": "g", "name": "Area"}},
                              {"id": "l-x", "name": "later", "parent": null}]}},
        {"identifier": "WEB-2", "cycle": {"id": "cy-1", "number": 1, "name": "Sprint A"},
         "labels": {"nodes": [{"id": "l-a", "name": "API", "parent": {"id": "g", "name": "Area"}}]}},
        {"identifier": "WEB-3", "projectMilestone": {"id": "ms", "name": "Beta"}}
    ]));
    let m = mapping(
        &[
            ("tab", "milestone"),
            ("column", "label-group:Area"),
            ("row", "cycle"),
        ],
        json!({"team": "WEB"}),
    );
    let tabs = group_board(
        &tickets,
        BoardGrouping::Configured {
            mapping: &m,
            space: None,
        },
    );
    assert_eq!(
        shape(&tabs),
        json!([
            ["Beta", [["No Area", [["No cycle", ["WEB-3"]]]]]],
            [
                "No milestone",
                [
                    ["API", [["Sprint A", ["WEB-2"]], ["Cycle 2", []]]],
                    ["Backend", [["Sprint A", []], ["Cycle 2", ["WEB-1"]]]]
                ]
            ]
        ])
    );
    assert_eq!(tabs[1].groups[0].key, "l-a");
    assert_eq!(tabs[1].groups[0].lanes[1].key, "cy-2");
}

#[test]
fn the_filter_compiles_to_a_linear_issue_filter_with_the_default_state_exclusion() {
    let m = mapping(
        &[("column", "state")],
        json!({"assignee": ["me", "Ana"], "team": "WEB", "priority": [1, 2], "parent": ["WEB-7", "uuid-x"]}),
    );
    assert_eq!(
        issue_filter(&m),
        json!({"and": [
            {"team": {"or": [{"id": {"in": ["WEB"]}}, {"key": {"in": ["WEB"]}}, {"name": {"in": ["WEB"]}}]}},
            {"assignee": {"or": [{"isMe": {"eq": true}}, {"id": {"in": ["Ana"]}}, {"name": {"in": ["Ana"]}},
                                 {"displayName": {"in": ["Ana"]}}, {"email": {"in": ["Ana"]}}]}},
            {"parent": {"or": [{"id": {"in": ["uuid-x"]}},
                               {"and": [{"team": {"key": {"eq": "WEB"}}}, {"number": {"eq": 7}}]}]}},
            {"state": {"type": {"nin": ["triage", "backlog"]}}},
            {"priority": {"in": [1, 2]}}
        ]})
    );

    let named_state = mapping(
        &[("column", "state")],
        json!({"state": "Todo", "label": "later"}),
    );
    assert_eq!(
        issue_filter(&named_state),
        json!({"and": [
            {"state": {"or": [{"id": {"in": ["Todo"]}}, {"name": {"in": ["Todo"]}}]}},
            {"labels": {"some": {"or": [{"id": {"in": ["later"]}}, {"name": {"in": ["later"]}}]}}}
        ]})
    );
    let typed = mapping(
        &[("column", "state")],
        json!({"state-type": "started", "assignee": "me"}),
    );
    assert_eq!(
        issue_filter(&typed),
        json!({"and": [
            {"assignee": {"isMe": {"eq": true}}},
            {"state": {"type": {"in": ["started"]}}}
        ]})
    );
}

#[test]
fn fill_snapshot_keeps_groups_filled_from_the_first_tab_for_old_clients() {
    let config = plugin_config();
    let tickets = plugin_tickets();
    let tabs = group_board(&tickets, choose(Some(&config), "WEB", None, &[]));
    let mut snapshot = LinearSnapshot::default();
    fill_snapshot(&mut snapshot, tabs.clone());
    assert_eq!(snapshot.tabs, tabs);
    assert_eq!(snapshot.groups, tabs[0].groups);

    let wire = serde_json::to_value(&snapshot).unwrap();
    let old_reader: Value = wire["groups"].clone();
    assert_eq!(old_reader[0]["label"], "Todo");
}

#[test]
fn a_snapshot_reads_explicit_null_tabs_and_lanes_as_empty() {
    let parsed: LinearSnapshot = serde_json::from_value(json!({
        "tabs": null,
        "groups": [{"key": "k", "label": "L", "issues": [], "lanes": null}]
    }))
    .expect("null tabs and lanes parse");
    assert!(parsed.tabs.is_empty());
    assert!(parsed.groups[0].lanes.is_empty());

    let nested: LinearSnapshot = serde_json::from_value(json!({
        "tabs": [{"key": null, "label": null, "groups": null}]
    }))
    .unwrap();
    assert_eq!(nested.tabs[0].key, "");
    assert!(nested.tabs[0].groups.is_empty());

    let old: LinearSnapshot = serde_json::from_value(json!({"groups": []})).unwrap();
    assert!(old.tabs.is_empty());
}
