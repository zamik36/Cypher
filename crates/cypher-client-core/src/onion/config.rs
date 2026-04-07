use super::cover::PowerMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorSettings {
    pub enabled: bool,
    pub bridge_lines: Vec<String>,
}

impl Default for TorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            bridge_lines: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonymousTransportConfig {
    pub power_mode: PowerMode,
    pub target_count: usize,
    pub tor: TorSettings,
}

impl Default for AnonymousTransportConfig {
    fn default() -> Self {
        Self {
            power_mode: PowerMode::Desktop,
            target_count: 3,
            tor: TorSettings::default(),
        }
    }
}
