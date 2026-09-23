#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingMode {
    Off,
    Cached,
    Remote,
}

impl PricingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PricingMode::Off => "off",
            PricingMode::Cached => "cached",
            PricingMode::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    pub tokscale_home: Option<String>,
    pub clients: Option<Vec<String>>,
    pub pricing: PricingMode,
    pub refresh_interval_secs: u64,
    pub use_env_roots: bool,
    /// Optional bearer token protecting `/api/*`. Absent = open (localhost use).
    pub api_token: Option<String>,
    /// Cloud sync target (the Worker's /api/ingest URL). Absent = sync off.
    pub sync_url: Option<String>,
    /// Shared ingest token sent as `Authorization: Bearer` on sync pushes.
    pub sync_token: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let pricing = match env_opt("TOKSCALE_PRICING").as_deref().unwrap_or("cached") {
            "off" => PricingMode::Off,
            "cached" => PricingMode::Cached,
            "remote" => PricingMode::Remote,
            other => {
                return Err(format!(
                    "invalid TOKSCALE_PRICING '{other}' (expected off|cached|remote)"
                ));
            }
        };
        let clients = env_opt("TOKSCALE_CLIENTS")
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());
        Ok(Self {
            bind_addr: env_opt("BIND_ADDR").unwrap_or_else(|| "127.0.0.1:8788".to_string()),
            tokscale_home: env_opt("TOKSCALE_HOME"),
            clients,
            pricing,
            refresh_interval_secs: env_opt("REFRESH_INTERVAL_SECS")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
            use_env_roots: env_opt("TOKSCALE_USE_ENV_ROOTS")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            api_token: env_opt("TOKSCALE_API_TOKEN"),
            sync_url: env_opt("SYNC_URL"),
            sync_token: env_opt("SYNC_TOKEN"),
        })
    }
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
