//! Separately authorized native orchestration tools. Persistence and application
//! validation belong to the repository port, never to a frontend or agent CLI.
use crate::{providers::inference::ToolDefinition, tools::ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NAMES: &[&str] = &[
    "child_create",
    "child_inspect",
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
    pub max_depth: u32,
    pub allowed_models: Vec<String>,
    pub allowed_tools: Vec<String>,
}

pub enum OrchestrationResult {
    Completed(ToolOutcome),
    Waiting,
}

pub fn validate(policy: &OrchestrationPolicy) -> bool {
    (1..=MAX_CHILDREN).contains(&policy.max_children)
        && (1..=MAX_DEPTH).contains(&policy.max_depth)
        && !policy.allowed_models.is_empty()
        && policy.allowed_models.len() <= 8
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

pub fn definitions(policy: &OrchestrationPolicy) -> Vec<ToolDefinition> {
    let id = json!({"type":"string","minLength":1,"maxLength":128});
    let tool_names = if policy.allowed_tools.is_empty() {
        json!({"type":"array","items":{"type":"string"},"maxItems":0})
    } else {
        json!({"type":"array","items":{"type":"string","enum":policy.allowed_tools},"maxItems":4})
    };
    let settings = json!({"type":"object","properties":{"max_output_tokens":{"type":["integer","null"],"minimum":1,"maximum":65536},"reasoning_effort":{"type":["string","null"]}},"additionalProperties":false});
    let budget = json!({"type":"object","properties":{"max_model_requests":{"type":"integer","minimum":0,"maximum":1000000},"max_tool_calls":{"type":"integer","minimum":0,"maximum":1000000}},"required":["max_model_requests","max_tool_calls"],"additionalProperties":false});
    let project = json!({"type":"object","properties":{"project_id":id,"base_ref":{"type":["string","null"]},"allowed_tools":tool_names},"required":["project_id","allowed_tools"],"additionalProperties":false});
    let mut spec = json!({"type":"object","properties":{"title":{"type":["string","null"]},"prompt":{"type":"string","minLength":1,"maxLength":65536},"model":{"type":"string","enum":policy.allowed_models},"settings":settings,"budget":budget},"required":["prompt","model"],"additionalProperties":false});
    if !policy.allowed_tools.is_empty() {
        spec["properties"]["project"] = project;
    }
    if policy.max_depth > 1 {
        spec["properties"]["orchestration"] = json!({"type":"object","properties":{"max_children":{"type":"integer","minimum":1,"maximum":policy.max_children},"max_depth":{"type":"integer","minimum":1,"maximum":policy.max_depth-1},"allowed_models":{"type":"array","items":{"type":"string","enum":policy.allowed_models},"maxItems":8},"allowed_tools":tool_names},"required":["max_children","max_depth","allowed_models","allowed_tools"],"additionalProperties":false});
    }
    let context = json!({"type":"object","properties":{"message_ids":{"type":"array","items":id,"maxItems":16},"artifact_ids":{"type":"array","items":id,"maxItems":16}},"additionalProperties":false});
    let mut result=vec![ToolDefinition{name:"child_create".into(),description:"Create one durable child with an explicit prompt, model, optional selected public context/artifacts, and isolated frozen-base project workspace. Returns stable task/run IDs. Authority cannot exceed the parent policy; no history is copied implicitly.".into(),parameters:json!({"type":"object","properties":{"spec":spec,"context":context,"cancel_with_parent":{"type":"boolean"}},"required":["spec"],"additionalProperties":false})}];
    for (name, description) in [
        (
            "child_inspect",
            "Inspect a direct child's task and latest attempt.",
        ),
        (
            "child_result",
            "Read a direct child's terminal attempt output without private provider continuation.",
        ),
        (
            "child_cancel",
            "Cancel a direct child's current unfinished attempt.",
        ),
    ] {
        result.push(ToolDefinition{name:name.into(),description:description.into(),parameters:json!({"type":"object","properties":{"task_id":id},"required":["task_id"],"additionalProperties":false})});
    }
    result.push(ToolDefinition{name:"child_wait".into(),description:"Wait for explicit direct-child run IDs. Releases execution capacity until every selected attempt is terminal, then returns their results. A retry is a different attempt and must be selected separately.".into(),parameters:json!({"type":"object","properties":{"child_run_ids":{"type":"array","items":id,"minItems":1,"maxItems":32,"uniqueItems":true}},"required":["child_run_ids"],"additionalProperties":false})});
    result
}

pub fn validate_arguments(name: &str, value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    match name {
        "child_create" => {
            object.get("spec").is_some_and(Value::is_object)
                && object
                    .keys()
                    .all(|k| matches!(k.as_str(), "spec" | "context" | "cancel_with_parent"))
        }
        "child_wait" => {
            object.len() == 1
                && object
                    .get("child_run_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|a| {
                        !a.is_empty()
                            && a.len() <= 32
                            && a.iter().all(|s| {
                                s.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 128)
                            })
                    })
        }
        "child_inspect" | "child_result" | "child_cancel" => {
            object.len() == 1
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
                max_depth: depth,
                allowed_models: vec!["opencode-go/glm-5.3-flash".into()],
                allowed_tools: vec![],
            };
            let definitions = definitions(&policy);
            for definition in &definitions {
                valid_ranges(&definition.parameters);
            }
            let spec = &definitions[0].parameters["properties"]["spec"];
            assert!(spec["properties"].get("project").is_none());
            if depth > 1 {
                assert_eq!(
                    spec["properties"]["orchestration"]["properties"]["allowed_tools"]["maxItems"],
                    0
                );
            }
            assert_eq!(spec["properties"].get("orchestration").is_some(), depth > 1);
        }
    }
}
