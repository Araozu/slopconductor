use super::*;

#[test]
fn provider_ids_round_trip() {
    for id in ProviderId::all() {
        assert_eq!(id.as_str().parse::<ProviderId>().as_ref(), Ok(id));
    }
    assert!("azure".parse::<ProviderId>().is_err());
}

#[test]
fn model_id_validation() {
    for valid in [
        "glm-5.3-flash",
        "muse-spark-1.3-contributor",
        "qwen3.8-max",
        "kimi-k2.7-code",
        "org/model:v1",
    ] {
        assert!(is_valid_model_id(valid), "{valid}");
    }
    for invalid in ["", "has space", "uniçode", "semi;colon", "quote\""] {
        assert!(!is_valid_model_id(invalid), "{invalid:?}");
    }
    assert!(!is_valid_model_id(&"m".repeat(MAX_MODEL_ID_LEN + 1)));
}

#[test]
fn model_ref_parse_and_display() {
    let parsed: ProviderModelRef = "opencode-go/glm-5.3-flash".parse().unwrap();
    assert_eq!(parsed.provider(), ProviderId::OpencodeGo);
    assert_eq!(parsed.model(), "glm-5.3-flash");
    assert_eq!(parsed.to_string(), "opencode-go/glm-5.3-flash");
    let zen: ProviderModelRef = "opencode-zen/claude-sonnet-4-6".parse().unwrap();
    assert_eq!(zen.provider(), ProviderId::OpencodeZen);
    assert_eq!(zen.model(), "claude-sonnet-4-6");
    assert_eq!(zen.to_string(), "opencode-zen/claude-sonnet-4-6");
    assert!("opencode-go/".parse::<ProviderModelRef>().is_err());
    assert!("no-slash".parse::<ProviderModelRef>().is_err());
    assert!("unknown/model".parse::<ProviderModelRef>().is_err());
}
