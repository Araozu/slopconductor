//! Separately authorized native orchestration tools. Persistence and application
//! validation belong to the repository port, never to a frontend or agent CLI.
use crate::{providers::inference::ToolDefinition, tools::ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NAMES: &[&str] = &[
    "child_create",
    "child_discover",
    "child_inspect",
    "child_send",
    "child_wait",
    "child_result",
    "child_cancel",
];
pub const MAX_CHILDREN: u32 = 32;
pub const MAX_DEPTH: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationPolicy {
    pub max_children: u32,
    #[serde(default = "default_concurrent_children")]
    pub max_concurrent_children: u32,
    pub max_depth: u32,
    pub allowed_models: Vec<String>,
    pub allowed_tools: Vec<String>,
}

fn default_concurrent_children() -> u32 {
    4
}

pub enum OrchestrationResult {
    Completed(ToolOutcome),
    Waiting,
}

pub fn validate(policy: &OrchestrationPolicy) -> bool {
    (1..=MAX_CHILDREN).contains(&policy.max_children)
        && (1..=8).contains(&policy.max_concurrent_children)
        && (1..=MAX_DEPTH).contains(&policy.max_depth)
        && policy
            .allowed_models
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == policy.allowed_models.len()
        && policy.allowed_tools.len() <= 4
        && policy
            .allowed_tools
            .iter()
            .all(|n| crate::tools::NAMES.contains(&n.as_str()))
        && policy
            .allowed_tools
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == policy.allowed_tools.len()
}

pub fn definitions(policy: &OrchestrationPolicy, workspace_id: &str) -> Vec<ToolDefinition> {
    let id = json!({"type":"string","minLength":1,"maxLength":128});
    let tool_names = if policy.allowed_tools.is_empty() {
        json!({"type":"array","items":{"type":"string"},"maxItems":0})
    } else {
        json!({"type":"array","items":{"type":"string","enum":policy.allowed_tools},"maxItems":4})
    };
    let settings = json!({"type":"object","properties":{"max_output_tokens":{"type":["integer","null"],"minimum":1,"maximum":65536},"reasoning_effort":{"type":["string","null"]},"full_context":{"type":["boolean","null"]}},"additionalProperties":false});
    let budget = json!({"type":"object","properties":{"max_model_requests":{"type":"integer","minimum":0,"maximum":1000000},"max_tool_calls":{"type":"integer","minimum":0,"maximum":1000000}},"required":["max_model_requests","max_tool_calls"],"additionalProperties":false});
    let mut workspace_id_schema = id.clone();
    workspace_id_schema["description"] = json!(format!(
        "Existing workspace ID. The parent uses '{workspace_id}'. Use 'default' with an empty allowed_tools list for a directory-less child. Other choices must be pre-created workspaces descended from the parent's workspace. Creating a child never creates a workspace or worktree."
    ));
    let workspace = json!({"type":"object","properties":{"workspace_id":workspace_id_schema,"allowed_tools":tool_names},"required":["workspace_id","allowed_tools"],"additionalProperties":false});
    let mut spec = json!({"type":"object","properties":{"title":{"type":["string","null"]},"prompt":{"type":"string","minLength":1,"maxLength":65536},"model":{"type":"string"},"settings":settings,"budget":budget},"required":["prompt","model","workspace"],"additionalProperties":false});
    if !policy.allowed_models.is_empty() {
        spec["properties"]["model"]["enum"] = json!(policy.allowed_models);
    }
    spec["properties"]["workspace"] = workspace;
    // Delegation is deliberately one level. Child policies are never offered
    // to a model, including for legacy parents whose stored policy had depth > 1.
    let context = json!({"type":"object","properties":{"message_ids":{"type":"array","items":id,"maxItems":16},"artifact_ids":{"type":"array","items":id,"maxItems":16}},"additionalProperties":false});
    let mut result=vec![ToolDefinition{name:"child_create".into(),description:"Create one durable child with an explicit prompt, model, optional selected public context/artifacts, and explicitly selected existing workspace; creation never allocates a worktree. Returns stable task/run IDs. Authority cannot exceed the parent policy; no history is copied implicitly.".into(),parameters:json!({"type":"object","properties":{"spec":spec,"context":context,"cancel_with_parent":{"type":"boolean"},"notification_mode":{"type":"string","enum":["completion","manual"]},"detached":{"type":"boolean"}},"required":["spec"],"additionalProperties":false})},ToolDefinition{name:"child_discover".into(),description:"List daemon-supported provider-qualified models and their capabilities and reasoning efforts. Use after and limit to page the catalog. This reports adapter support, not account entitlement; credentials are never included.".into(),parameters:json!({"type":"object","properties":{"after":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":32}},"additionalProperties":false})}];
    for (name, description) in [
        (
            "child_inspect",
            "Inspect bounded child progress and public event summaries after an optional event cursor.",
        ),
        (
            "child_send",
            "Send an instruction to an owned direct child using durable next-boundary or after-turn delivery.",
        ),
        (
            "child_result",
            "Read a direct child's latest terminal output once all its turns have finished, without private provider continuation.",
        ),
        (
            "child_cancel",
            "Cancel every unfinished turn of a direct child, including queued follow-ups.",
        ),
    ] {
        let parameters = if name == "child_send" {
            json!({"type":"object","properties":{"task_id":id,"text":{"type":"string","minLength":1,"maxLength":65536},"delivery":{"type":"string","enum":["next-boundary","after-turn"]}},"required":["task_id","text"],"additionalProperties":false})
        } else if name == "child_inspect" {
            json!({"type":"object","properties":{"task_id":id,"event_after":{"type":"integer","minimum":0},"event_limit":{"type":"integer","minimum":1,"maximum":16}},"required":["task_id"],"additionalProperties":false})
        } else {
            json!({"type":"object","properties":{"task_id":id},"required":["task_id"],"additionalProperties":false})
        };
        result.push(ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters,
        });
    }
    result.push(ToolDefinition{name:"child_wait".into(),description:"Join explicit direct-child run IDs, or all direct children when omitted. Supports all/any completion and a bounded timeout; timed-out children remain running.".into(),parameters:json!({"type":"object","properties":{"child_run_ids":{"type":"array","items":id,"maxItems":32,"uniqueItems":true},"mode":{"type":"string","enum":["all","any"]},"timeout_ms":{"type":"integer","minimum":1,"maximum":86400000}},"additionalProperties":false})});
    result
}

pub fn validate_arguments(name: &str, value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    match name {
        "child_create" => {
            object.get("spec").is_some_and(Value::is_object)
                && object.keys().all(|k| {
                    matches!(
                        k.as_str(),
                        "spec"
                            | "context"
                            | "cancel_with_parent"
                            | "notification_mode"
                            | "detached"
                    )
                })
                && object
                    .get("notification_mode")
                    .is_none_or(|v| matches!(v.as_str(), Some("completion" | "manual")))
                && object.get("detached").is_none_or(Value::is_boolean)
        }
        "child_discover" => {
            object
                .keys()
                .all(|k| matches!(k.as_str(), "after" | "limit"))
                && object
                    .get("after")
                    .is_none_or(|v| v.as_str().is_some_and(|s| s.len() <= 384))
                && object
                    .get("limit")
                    .is_none_or(|v| v.as_u64().is_some_and(|n| (1..=32).contains(&n)))
        }
        "child_wait" => {
            object
                .keys()
                .all(|k| matches!(k.as_str(), "child_run_ids" | "mode" | "timeout_ms"))
                && object.get("child_run_ids").is_none_or(|v| {
                    v.as_array().is_some_and(|a| {
                        a.len() <= 32
                            && a.iter().all(|s| {
                                s.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 128)
                            })
                    })
                })
                && object
                    .get("mode")
                    .is_none_or(|v| matches!(v.as_str(), Some("all" | "any")))
                && object
                    .get("timeout_ms")
                    .is_none_or(|v| v.as_u64().is_some_and(|n| (1..=86_400_000).contains(&n)))
        }
        "child_send" => {
            object
                .keys()
                .all(|k| matches!(k.as_str(), "task_id" | "text" | "delivery"))
                && object
                    .get("task_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
                && object
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 65536)
                && object
                    .get("delivery")
                    .is_none_or(|v| matches!(v.as_str(), Some("next-boundary" | "after-turn")))
        }
        "child_inspect" | "child_result" | "child_cancel" => {
            object
                .keys()
                .all(|k| matches!(k.as_str(), "task_id" | "event_after" | "event_limit"))
                && object
                    .get("event_after")
                    .is_none_or(|v| v.as_u64().is_some())
                && object
                    .get("event_limit")
                    .is_none_or(|v| v.as_u64().is_some_and(|n| (1..=16).contains(&n)))
                && (name == "child_inspect" || object.len() == 1)
                && object
                    .get("task_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Empty enums and impossible ranges are rejected by provider schema
    // validators even when the offending property is optional.
    fn valid_ranges(value: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(Value::Array(values)) = object.get("enum") {
                    assert!(!values.is_empty());
                }
                for (low, high) in [("minimum", "maximum"), ("minItems", "maxItems")] {
                    if let (Some(low), Some(high)) = (object.get(low), object.get(high)) {
                        assert!(low.as_u64().unwrap() <= high.as_u64().unwrap());
                    }
                }
                object.values().for_each(valid_ranges);
            }
            Value::Array(values) => values.iter().for_each(valid_ranges),
            _ => {}
        }
    }

    #[test]
    fn text_only_and_last_depth_tool_schemas_have_satisfiable_bounds() {
        for depth in 1..=MAX_DEPTH {
            let policy = OrchestrationPolicy {
                max_children: 4,
                max_concurrent_children: 2,
                max_depth: depth,
                allowed_models: vec!["opencode-go/glm-5.3-flash".into()],
                allowed_tools: vec![],
            };
            let definitions = definitions(&policy, "parent-workspace");
            for definition in &definitions {
                valid_ranges(&definition.parameters);
            }
            let spec = &definitions[0].parameters["properties"]["spec"];
            assert!(spec["properties"].get("workspace").is_some());
            let workspace_description =
                spec["properties"]["workspace"]["properties"]["workspace_id"]["description"]
                    .as_str()
                    .unwrap();
            assert!(workspace_description.contains("parent-workspace"));
            assert!(workspace_description.contains("'default'"));
            assert!(spec["properties"].get("orchestration").is_none());
        }
    }
}
