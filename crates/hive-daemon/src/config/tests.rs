use std::{fs, path::Path};

use super::*;

fn env(root: &Path) -> ConfigEnvironment {
    ConfigEnvironment {
        xdg_config_home: Some(root.join("cfg")),
        xdg_data_home: Some(root.join("data")),
        home: Some(root.join("home")),
        local_app_data: Some(root.join("local")),
    }
}

#[test]
#[cfg(not(windows))]
fn relative_xdg_paths_are_ignored_and_overrides_are_absolute() {
    let temp = tempfile::tempdir().unwrap();
    let mut env = env(temp.path());
    env.xdg_data_home = Some("relative".into());
    let cfg = load_with_environment(
        StartupArgs {
            peer_listen: None,
            listen: None,
            config: None,
            data_dir: None,
            provider_base_url: None,
            opencode_zen_base_url: None,
            codex_base_url: None,
            default_model: None,
        },
        &env,
    )
    .unwrap();
    assert_eq!(
        cfg.data_dir,
        temp.path().join("home/.local/share/slopconductor")
    );
    let error = load_with_environment(
        StartupArgs {
            peer_listen: None,
            listen: None,
            config: None,
            data_dir: Some("relative".into()),
            provider_base_url: None,
            opencode_zen_base_url: None,
            codex_base_url: None,
            default_model: None,
        },
        &env,
    )
    .unwrap_err();
    assert!(error.to_string().contains("absolute"));
}

#[test]
fn explicit_config_must_exist_and_file_settings_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let environment = env(temp.path());
    let missing = temp.path().join("missing.toml");
    assert!(
        load_with_environment(
            StartupArgs {
                peer_listen: None,
                listen: None,
                config: Some(missing),
                data_dir: None,
                provider_base_url: None,
                opencode_zen_base_url: None,
                codex_base_url: None,
                default_model: None
            },
            &environment
        )
        .is_err()
    );
    let config_path = temp.path().join("settings.toml");
    fs::write(&config_path, "listen = '0.0.0.0:7331'\n").unwrap();
    assert!(
        load_with_environment(
            StartupArgs {
                peer_listen: None,
                listen: None,
                config: Some(config_path),
                data_dir: None,
                provider_base_url: None,
                opencode_zen_base_url: None,
                codex_base_url: None,
                default_model: None
            },
            &environment
        )
        .is_err()
    );
}

#[test]
fn explicit_overrides_and_absolute_xdg_paths_do_not_require_home() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("explicit.toml");
    fs::write(&config_path, "").unwrap();
    let environment = ConfigEnvironment::default();
    let cfg = load_with_environment(
        StartupArgs {
            peer_listen: None,
            listen: None,
            config: Some(config_path),
            data_dir: Some(temp.path().join("data")),
            provider_base_url: None,
            opencode_zen_base_url: None,
            codex_base_url: None,
            default_model: None,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(cfg.data_dir, temp.path().join("data"));

    #[cfg(not(windows))]
    {
        let environment = ConfigEnvironment {
            xdg_config_home: Some(temp.path().join("cfg")),
            xdg_data_home: Some(temp.path().join("data")),
            ..ConfigEnvironment::default()
        };
        assert!(
            load_with_environment(
                StartupArgs {
                    peer_listen: None,
                    listen: None,
                    config: None,
                    data_dir: None,
                    provider_base_url: None,
                    opencode_zen_base_url: None,
                    codex_base_url: None,
                    default_model: None
                },
                &environment
            )
            .is_ok()
        );
    }
}
