//! Provider-specific context limits from the OpenCode-maintained models.dev catalog.
//! Snapshot: https://github.com/anomalyco/models.dev/tree/74dac73b3587dde8032641951d87530819dbb4e8
//! Checked 2026-10-13. Unknown limits remain unknown; never infer from family names.
use slop_core::provider::ProviderId;

pub(super) fn limits(provider: ProviderId, model: &str) -> (Option<u64>, Option<u64>) {
    let limits = match (provider, model) {
        (ProviderId::OpencodeGo, "grok-4.7" | "grok-4.6") => (500_000, None),
        (ProviderId::OpencodeGo, "gpt-6-luna" | "gpt-5.6-luna") => (1_050_000, Some(922_000)),
        (
            ProviderId::OpencodeGo,
            "muse-spark-1.3-contributor"
            | "muse-spark-1.2-contributor"
            | "kimi-k3"
            | "mimo-v2.6-flash"
            | "mimo-v2.6-pro"
            | "mimo-v2.5-pro",
        ) => (1_048_576, None),
        (
            ProviderId::OpencodeGo,
            "claude-haiku-5-5"
            | "minimax-m3"
            | "qwen3.8-max"
            | "qwen3.8-flash"
            | "qwen3.7-plus"
            | "glm-5.3-flash"
            | "glm-5.3"
            | "glm-5.2"
            | "longcat-2.0"
            | "longcat-2.5-preview-free"
            | "deepseek-v4.1-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "mimo-v2.5",
        ) => (1_000_000, None),
        (ProviderId::OpencodeGo, "minimax-m2.7") => (204_800, None),
        (ProviderId::OpencodeGo, "kimi-k2.7-code" | "kimi-k2.6") => (262_144, None),
        (ProviderId::OpencodeGo, "step-5-preview-free") => (1_000_000, Some(1_000_000)),
        (ProviderId::OpencodeGo, "hy4-preview") => (1_024_000, None),
        (ProviderId::OpencodeGo, "hy3") => (256_000, Some(192_000)),
        (ProviderId::OpencodeGo, "space-bunny") => (1_048_576, Some(524_288)),
        (
            ProviderId::OpencodeZen,
            "gpt-6-astra" | "gpt-6-sol" | "gpt-6.1-sol" | "gpt-6-luna" | "gpt-5.6-sol"
            | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-5.5" | "gpt-5.5-pro" | "gpt-5.4"
            | "gpt-5.4-pro",
        ) => (1_050_000, Some(922_000)),
        (
            ProviderId::OpencodeZen,
            "gpt-5.4-mini" | "gpt-5.4-nano" | "gpt-5.3-codex" | "gpt-5.2" | "gpt-5.2-codex"
            | "gpt-5.1" | "gpt-5.1-codex" | "gpt-5.1-codex-max" | "gpt-5.1-codex-mini" | "gpt-5"
            | "gpt-5-codex" | "gpt-5-nano",
        ) => (400_000, Some(272_000)),
        (ProviderId::OpencodeZen, "gpt-5.3-codex-spark") => (128_000, Some(128_000)),
        (ProviderId::OpencodeZen, "grok-4.7" | "grok-4.6" | "grok-4.5") => (500_000, None),
        (ProviderId::OpencodeZen, "grok-build-0.1") => (256_000, None),
        (
            ProviderId::OpencodeZen,
            "muse-spark-1.3"
            | "muse-spark-1.2"
            | "muse-spark-1.3-contributor-free"
            | "kimi-k3"
            | "exo-free",
        ) => (1_048_576, None),
        (
            ProviderId::OpencodeZen,
            "claude-fable-5-1"
            | "claude-fable-5"
            | "claude-opus-5-5"
            | "claude-opus-5"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5-5"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
            | "claude-sonnet-4-5"
            | "claude-haiku-5-5"
            | "qwen3.8-flash"
            | "deepseek-v4.1-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "glm-5.3-flash"
            | "glm-5.3"
            | "glm-5.2"
            | "longcat-2.5-preview-free"
            | "nemotron-3-ultra-free",
        ) => (1_000_000, None),
        (
            ProviderId::OpencodeZen,
            "claude-opus-4-5" | "claude-haiku-4-5" | "mimo-v2.6-flash-free" | "mimo-v2.5-free",
        ) => (200_000, None),
        (
            ProviderId::OpencodeZen,
            "qwen3.6-plus"
            | "qwen3.5-plus"
            | "qwen3.8-max"
            | "kimi-k2.5"
            | "kimi-k2.6"
            | "kimi-k2.7-code"
            | "ling-3.1-flash-free"
            | "ling-3.0-flash-fin-free"
            | "nemotron-3.5-lightning-free",
        ) => (262_144, None),
        (ProviderId::OpencodeZen, "minimax-m3") => (512_000, None),
        (ProviderId::OpencodeZen, "minimax-m2.7" | "minimax-m2.5" | "glm-5.1" | "glm-5") => {
            (204_800, None)
        }
        (ProviderId::OpencodeZen, "mistral-large-4") => (524_288, None),
        (ProviderId::OpencodeZen, "big-pickle") => (200_000, Some(160_000)),
        (ProviderId::OpencodeZen, "space-bunny-free") => (1_048_576, Some(524_288)),
        (ProviderId::OpencodeZen, "step-5-preview-free") => (1_000_000, Some(1_000_000)),
        // Official OpenAI GPT-6 model pages specify a 1,050,000-token window.
        (ProviderId::Codex, "gpt-6.1-sol" | "gpt-6-sol" | "gpt-6-luna" | "gpt-6-astra") => {
            (1_050_000, None)
        }
        _ => return (None, None),
    };
    (Some(limits.0), limits.1)
}
