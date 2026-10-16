//! Completed Responses items can arrive before a terminal event whose output
//! array is empty. Retain those snapshots; deltas alone are not final evidence.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{inference::MAX_BLOCKS, opencode::DecodeError};

#[derive(Debug, Default)]
pub(super) struct CompletedOutput {
    items: BTreeMap<usize, Value>,
    bytes: usize,
}

impl CompletedOutput {
    pub(super) fn observe(&mut self, event: &Value) -> Result<(), DecodeError> {
        if event.get("type").and_then(Value::as_str) != Some("response.output_item.done") {
            return Ok(());
        }
        let invalid = |detail| DecodeError::InvalidResponse { detail };
        let index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .filter(|index| *index < MAX_BLOCKS as u64)
            .ok_or_else(|| invalid("invalid completed output index"))? as usize;
        let item = event
            .get("item")
            .ok_or_else(|| invalid("completed output item is missing"))?;
        if !matches!(
            item.get("type").and_then(Value::as_str),
            Some("message" | "function_call" | "reasoning")
        ) {
            return Err(DecodeError::UnsupportedCapability {
                capability: "unsupported Responses output item",
            });
        }
        if self.items.contains_key(&index) {
            return Err(invalid("duplicate completed output item"));
        }
        self.bytes = self.bytes.saturating_add(
            crate::json::encoded_len(item).map_err(|_| invalid("invalid completed output item"))?,
        );
        if self.bytes > super::opencode::MAX_BODY_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "completed output exceeds byte bound",
            });
        }
        self.items.insert(index, item.clone());
        Ok(())
    }

    pub(super) fn complete(&mut self, response: &mut Value) -> Result<(), DecodeError> {
        let sparse = match response.get("output") {
            None | Some(Value::Null) => true,
            Some(Value::Array(output)) => output.is_empty(),
            _ => {
                return Err(DecodeError::InvalidResponse {
                    detail: "invalid terminal output array",
                });
            }
        };
        if !sparse {
            self.items.clear();
            self.bytes = 0;
            return Ok(());
        }
        if self.items.is_empty() {
            return Ok(());
        }
        if self
            .items
            .keys()
            .enumerate()
            .any(|(expected, index)| expected != *index)
        {
            return Err(DecodeError::InvalidResponse {
                detail: "missing completed output item",
            });
        }
        response["output"] = std::mem::take(&mut self.items).into_values().collect();
        self.bytes = 0;
        Ok(())
    }
}
