use std::{env, path::PathBuf};

#[derive(Clone, Debug, Default)]
pub struct ConfigEnvironment {
    #[cfg_attr(windows, allow(dead_code))]
    pub xdg_config_home: Option<PathBuf>,
    #[cfg_attr(windows, allow(dead_code))]
    pub xdg_data_home: Option<PathBuf>,
    #[cfg_attr(windows, allow(dead_code))]
    pub home: Option<PathBuf>,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub local_app_data: Option<PathBuf>,
}

impl ConfigEnvironment {
    pub fn current() -> Self {
        Self {
            xdg_config_home: env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            xdg_data_home: env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
            local_app_data: env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}
