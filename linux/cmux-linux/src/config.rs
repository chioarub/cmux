use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub socket_path: Option<String>,
    pub scrollback_lines: i64,
    pub sidebar_width: i32,
    pub font_family: String,
    pub font_size: u32,
    pub color_scheme: ColorScheme,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ColorScheme {
    System,
    Dark,
    Light,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket_path: None,
            scrollback_lines: 20_000,
            sidebar_width: 280,
            font_family: "Monospace".to_string(),
            font_size: 12,
            color_scheme: ColorScheme::System,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!("cmux: failed to parse {}: {err}", path.display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn font_description(&self) -> String {
        format!("{} {}", self.font_family, self.font_size)
    }
}

fn config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join("cmux/config.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/cmux/config.toml")
    } else {
        PathBuf::from("/tmp/cmux-config.toml")
    }
}
