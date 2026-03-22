use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default = "default_gateway_addr")]
    pub gateway_addr: String,
    #[serde(default = "default_signaling_addr")]
    pub signaling_addr: String,
    #[serde(default = "default_relay_addr")]
    pub relay_addr: String,
    #[serde(default = "default_ws_addr")]
    pub ws_addr: String,
    #[serde(default = "default_stun_addr")]
    pub stun_addr: String,
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(default = "default_nats_url")]
    pub nats_url: String,
}

impl AppConfig {
    pub fn load() -> anyhow::Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::Environment::with_prefix("P2P"))
            .build()?;
        Ok(cfg.try_deserialize()?)
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gateway_addr: default_gateway_addr(),
            signaling_addr: default_signaling_addr(),
            relay_addr: default_relay_addr(),
            ws_addr: default_ws_addr(),
            stun_addr: default_stun_addr(),
            redis_url: default_redis_url(),
            nats_url: default_nats_url(),
        }
    }
}

fn default_gateway_addr() -> String {
    "0.0.0.0:9100".into()
}
fn default_signaling_addr() -> String {
    "0.0.0.0:9200".into()
}
fn default_relay_addr() -> String {
    "0.0.0.0:9300".into()
}
fn default_ws_addr() -> String {
    "0.0.0.0:9101".into()
}
fn default_stun_addr() -> String {
    "0.0.0.0:3478".into()
}
fn default_redis_url() -> String {
    "redis://127.0.0.1:6379".into()
}
fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".into()
}
