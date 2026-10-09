use std::io::{self, Write};

use slop_protocol::chat::{CommandReceipt, EventFrame, MessageResponse, TurnResponse};

use crate::Result;

#[derive(Debug, Clone, Copy)]
pub enum Output {
    Human,
    Json,
}

impl Output {
    pub fn new(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }

    pub fn value<T: serde::Serialize>(self, value: &T, human: impl FnOnce()) -> Result<()> {
        match self {
            Self::Json => println!("{}", serde_json::to_string(value)?),
            Self::Human => human(),
        }
        Ok(())
    }

    pub fn session_receipt(self, receipt: &CommandReceipt) {
        if matches!(self, Self::Json) {
            println!(
                "{}",
                serde_json::json!({"type":"session_receipt","receipt":receipt})
            );
        }
    }

    pub fn message_receipt(self, receipt: &CommandReceipt, detach: bool) {
        match self {
            Self::Json => println!(
                "{}",
                serde_json::json!({"type":"receipt","receipt":receipt})
            ),
            Self::Human if detach => println!(
                "accepted session {} turn {} command {}",
                receipt.session_id,
                receipt.turn_id.as_deref().unwrap_or("pending"),
                receipt.command_id
            ),
            Self::Human => (),
        }
    }

    pub fn delta(self, visible: &str) -> Result<()> {
        if matches!(self, Self::Human) {
            print!("{visible}");
            io::stdout().flush()?;
        }
        Ok(())
    }

    pub fn frame(self, frame: &EventFrame) -> Result<()> {
        if matches!(self, Self::Json) {
            println!("{}", serde_json::to_string(frame)?);
        }
        Ok(())
    }

    pub fn terminal(self, turn: &TurnResponse, message: Option<&MessageResponse>, shown: &str) {
        match self {
            Self::Json => println!(
                "{}",
                serde_json::json!({"type":"terminal","turn":turn,"message":message})
            ),
            Self::Human => {
                if let Some(message) = message
                    && !message.text.is_empty()
                {
                    if shown.is_empty() {
                        println!("{}", message.text);
                    } else if message.text.starts_with(shown) {
                        print!("{}", &message.text[shown.len()..]);
                        println!();
                    } else {
                        eprintln!("\n[canonical assistant reply updated]");
                        println!("{}", message.text);
                    }
                }
                if turn.status != "completed" {
                    eprintln!("turn {}", turn.status);
                }
            }
        }
    }

    pub fn canonical(self, turn: &TurnResponse, message: Option<&MessageResponse>, shown: &str) {
        match self {
            Self::Json => println!(
                "{}",
                serde_json::json!({"type":"canonical","turn":turn,"message":message})
            ),
            Self::Human => {
                if let Some(message) = message {
                    if message.text.starts_with(shown) {
                        print!("{}", &message.text[shown.len()..]);
                        println!();
                    } else if shown.is_empty() {
                        println!("{}", message.text);
                    } else {
                        eprintln!("\n[canonical assistant reply updated]");
                        println!("{}", message.text);
                    }
                }
            }
        }
    }
}
