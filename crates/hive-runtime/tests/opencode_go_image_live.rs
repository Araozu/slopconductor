//! Live OpenCode Go image-input checks. Run with `OPENCODE_GO_API_KEY` set:
//!
//! ```sh
//! set -a; source .env; set +a
//! cargo test -p hive-runtime --test opencode_go_image_live --locked -- --ignored
//! ```
//!
//! Ignored by default: choose credentials and a live test budget explicitly.
//! The suite makes three inference requests (one per wire shape) with output
//! caps of 512 (Chat), 64 (Messages), and 512 (Responses) tokens; these are
//! not billing caps. An explicitly selected live run fails if its credential
//! is missing.

use std::collections::HashMap;
use std::sync::Arc;

use hive_runtime::providers::{
    ProviderClient, WireProtocol,
    inference::{
        BlockContent, ContentBlock, FinishReason, GenerationSettings, InferenceMessage,
        InferenceRequest, MessageRole, ResolvedImage,
    },
    opencode_go::OpencodeGoClient,
};

fn live_client() -> Box<dyn ProviderClient> {
    Box::new(OpencodeGoClient::from_env().expect("live checks require OPENCODE_GO_API_KEY"))
}

/// Well-formed content-addressed id for the generated fixture. `infer` only
/// needs the id to be valid hex present in the request's side map; comparing
/// the id against the byte hash happens daemon-side at dispatch (covered by
/// the daemon artifact tests), never on this path.
const RED_ARTIFACT_ID: &str = "ab00000000000000000000000000000000000000000000000000000000000000";

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = crc & 1;
            crc >>= 1;
            if mask == 1 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn chunk(chunk_type: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut keyed = Vec::with_capacity(4 + data.len());
    keyed.extend_from_slice(chunk_type);
    keyed.extend_from_slice(data);
    out.extend_from_slice(&crc32(&keyed).to_be_bytes());
}

/// Minimal deterministic PNG encoder: 8-bit truecolor, no interlace. Raw
/// scanlines travel in a single zlib "stored" block so the test needs no image
/// dependency.
fn solid_red_png(edge: u32) -> Vec<u8> {
    let mut row = vec![0u8; 1 + edge as usize * 3]; // filter type 0 (None)
    for pixel in row[1..].as_chunks_mut::<3>().0 {
        pixel.copy_from_slice(&[255, 0, 0]);
    }
    let mut raw = Vec::with_capacity(row.len() * edge as usize);
    for _ in 0..edge {
        raw.extend_from_slice(&row);
    }
    assert!(
        raw.len() <= 65_535,
        "stored zlib block holds a 64x64 RGB square"
    );
    let mut zlib = vec![0x78, 0x01, 0x01]; // header + final stored block
    zlib.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&edge.to_be_bytes());
    ihdr.extend_from_slice(&edge.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit truecolor
    chunk(b"IHDR", &ihdr, &mut png);
    chunk(b"IDAT", &zlib, &mut png);
    chunk(b"IEND", &[], &mut png);
    png
}

fn image_request(model: &str, max_tokens: u32) -> InferenceRequest {
    let bytes: Arc<[u8]> = Arc::from(solid_red_png(64).into_boxed_slice());
    let mut images = HashMap::new();
    images.insert(
        RED_ARTIFACT_ID.to_owned(),
        ResolvedImage {
            media_type: "image/png".to_owned(),
            bytes,
        },
    );
    InferenceRequest {
        request_id: format!("slop-image-live-{}", model.replace('.', "-")),
        session_id: format!("slop-image-live-{}", model.replace('.', "-")),
        model: model.to_owned(),
        settings: GenerationSettings {
            max_output_tokens: Some(max_tokens),
            reasoning_effort: None,
            full_context: None,
        },
        messages: vec![InferenceMessage {
            role: MessageRole::User,
            blocks: vec![
                ContentBlock {
                    id: "text-block".to_owned(),
                    content: BlockContent::Text {
                        text:
                            "What single colour fills this image? Answer with one lowercase word."
                                .to_owned(),
                    },
                },
                ContentBlock {
                    id: "image-block".to_owned(),
                    content: BlockContent::Image {
                        artifact_id: RED_ARTIFACT_ID.to_owned(),
                        media_type: "image/png".to_owned(),
                    },
                },
            ],
            continuation: None,
        }],
        tools: Vec::new(),
        images,
    }
}

async fn assert_model_sees_red(model: &str, max_tokens: u32, wire: WireProtocol) {
    let client = live_client();
    assert_eq!(
        client
            .descriptor()
            .wire_protocol(model)
            .expect("fixture model stays in the compiled catalog"),
        wire,
        "{model} must stay on its documented wire"
    );
    assert!(
        client.capabilities(model).image_input,
        "{model} must advertise image_input; pick another {wire:?} model if this fails"
    );
    let request = image_request(model, max_tokens);
    let response = client
        .infer(&request, &mut |_| {})
        .await
        .expect("image inference");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    let text = response.message.visible_text();
    assert!(!text.trim().is_empty(), "{model} returned no visible text");
    assert!(
        text.to_lowercase().contains("red"),
        "{model} did not see red; answer was {text:?}"
    );
}

#[test]
fn red_png_encoder_emits_a_solid_square() {
    let png = solid_red_png(64);
    assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    // IHDR must describe a 64x64 8-bit truecolor image.
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(&png[16..24], &[0, 0, 0, 64, 0, 0, 0, 64]);
    assert_eq!(&png[24..29], &[8, 2, 0, 0, 0]);
    // Re-derive the raw scanlines from our own stored zlib block.
    let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
    assert_eq!(&png[37..41], b"IDAT");
    let zlib = &png[41..41 + idat_len];
    assert_eq!(&zlib[..3], &[0x78, 0x01, 0x01]);
    let len = u16::from_le_bytes([zlib[3], zlib[4]]) as usize;
    assert_eq!(u16::from_le_bytes([zlib[5], zlib[6]]), !len as u16);
    let raw = &zlib[7..7 + len];
    assert_eq!(
        u32::from_be_bytes(zlib[7 + len..11 + len].try_into().unwrap()),
        adler32(raw)
    );
    assert_eq!(raw.len(), 64 * (1 + 64 * 3));
    for row in raw.as_chunks::<{ 1 + 64 * 3 }>().0 {
        assert_eq!(row[0], 0);
        for pixel in row[1..].as_chunks::<3>().0 {
            assert_eq!(pixel, &[255, 0, 0]);
        }
    }
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_responses_image_turn() {
    assert_model_sees_red(
        "muse-spark-1.3-contributor",
        512,
        WireProtocol::OpenAiResponses,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_chat_completions_image_turn() {
    assert_model_sees_red("glm-5.3-flash", 512, WireProtocol::OpenAiChatCompletions).await;
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_anthropic_messages_image_turn() {
    assert_model_sees_red("claude-haiku-5-5", 64, WireProtocol::AnthropicMessages).await;
}
