use std::error::Error;

use slop_client::{ClientError, new_command_id};

use crate::Result;

pub fn mutation_error(error: ClientError, command_id: &str) -> Box<dyn Error + Send + Sync> {
    match error {
        ClientError::DeliveryUncertain(message) => {
            format!("{message}; retry the same operation with --command-id {command_id}").into()
        }
        other => Box::new(other),
    }
}

pub fn choose_command_id(value: Option<String>) -> Result<String> {
    match value {
        Some(value) => Ok(value),
        None => Ok(new_command_id()?),
    }
}
