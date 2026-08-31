use serde::Deserialize;
use std::path::Path;

/// Which wire format an endpoint speaks. This is about *parsing* — how the
/// request and response bodies are shaped — and says nothing about who is
/// serving them: a local llama.cpp server speaking `openai` is handled by
/// exactly the same code path as api.openai.com.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiStyle {
    OpenAI,
    Anthropic,
}

impl ApiStyle {
    /// The path a client of this style appends to its configured base URL,
    /// and therefore the path appended to a provider's `base_url` here.
    fn endpoint_path(&self) -> &'static str {
        match self {
            ApiStyle::OpenAI => "/chat/completions",
            ApiStyle::Anthropic => "/v1/messages",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ApiStyle::OpenAI => "openai",
            ApiStyle::Anthropic => "anthropic",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub api: ApiStyle,
    pub base_url: String,
    /// The provider the bare `/v1/chat/completions` and `/v1/messages`
    /// routes resolve to, for clients that can set neither a path prefix
    /// nor a header. At most one per api style is meaningful; the first
    /// marked one wins.
    #[serde(default)]
    pub default: bool,
}

impl ProviderConfig {
    /// The full URL a call to this provider is forwarded to. A `base_url`
    /// that already names the endpoint is left alone, so pasting a complete
    /// URL out of another tool's config also works.
    pub fn target_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let path = self.api.endpoint_path();
        if base.ends_with(path) {
            base.to_string()
        } else {
            format!("{base}{path}")
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ProvidersFile {
    providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone)]
pub struct ProviderTable {
    providers: Vec<ProviderConfig>,
}

/// The hosted APIs, used when `providers.yaml` is missing, unparseable, or
/// simply doesn't mention one of the two styles. Keeping these as a floor
/// means a broken config degrades to the previous hardcoded behavior rather
/// than to a proxy that can't forward anything.
fn builtin(style: ApiStyle) -> ProviderConfig {
    let (name, base_url) = match style {
        ApiStyle::OpenAI => ("openai", "https://api.openai.com/v1"),
        ApiStyle::Anthropic => ("anthropic", "https://api.anthropic.com"),
    };
    ProviderConfig {
        name: name.to_string(),
        api: style,
        base_url: base_url.to_string(),
        default: true,
    }
}

impl ProviderTable {
    pub fn load(path: &Path) -> Self {
        let providers = match std::fs::read_to_string(path) {
            Ok(content) => match Self::parse(&content) {
                Ok(table) => return table,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to parse {} ({e}); falling back to the hosted provider defaults",
                        path.display()
                    );
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self::from_configs(providers)
    }

    pub fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        let file: ProvidersFile = serde_yaml::from_str(yaml)?;
        Ok(Self::from_configs(file.providers))
    }

    fn from_configs(mut providers: Vec<ProviderConfig>) -> Self {
        // Guarantee a usable entry for both styles, so resolution can never
        // fail for the bare routes no matter what the file says.
        for style in [ApiStyle::OpenAI, ApiStyle::Anthropic] {
            if !providers.iter().any(|p| p.api == style) {
                providers.push(builtin(style));
            }
        }
        Self { providers }
    }

    /// Env overrides for the two default endpoints, applied after loading.
    /// Deliberately *not* `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL`: those are
    /// usually already set to point a client *at this proxy*, and honoring
    /// them here would make Harnesswurm forward to itself.
    pub fn apply_env_overrides(&mut self) {
        for (var, style) in [
            ("HARNESSWURM_OPENAI_BASE_URL", ApiStyle::OpenAI),
            ("HARNESSWURM_ANTHROPIC_BASE_URL", ApiStyle::Anthropic),
        ] {
            let Ok(url) = std::env::var(var) else { continue };
            if url.trim().is_empty() {
                continue;
            }
            let index = self.default_index(style);
            self.providers[index].base_url = url.trim().to_string();
        }
    }

    fn default_index(&self, style: ApiStyle) -> usize {
        self.providers
            .iter()
            .position(|p| p.api == style && p.default)
            .or_else(|| self.providers.iter().position(|p| p.api == style))
            .expect("from_configs guarantees an entry per api style")
    }

    /// The provider a bare `/v1/chat/completions` or `/v1/messages` call
    /// goes to.
    pub fn default_for(&self, style: ApiStyle) -> &ProviderConfig {
        &self.providers[self.default_index(style)]
    }

    pub fn by_name(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name.as_str()).collect()
    }

    pub fn all(&self) -> &[ProviderConfig] {
        &self.providers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(yaml: &str) -> ProviderTable {
        ProviderTable::parse(yaml).expect("test yaml parses")
    }

    #[test]
    fn target_url_appends_the_style_endpoint() {
        let t = table(
            "providers:\n  - name: ollama\n    api: openai\n    base_url: http://localhost:11434/v1\n",
        );
        assert_eq!(
            t.by_name("ollama").unwrap().target_url(),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn target_url_tolerates_a_trailing_slash_and_a_full_endpoint_url() {
        let t = table(
            "providers:\n\
             \x20 - name: slashed\n    api: anthropic\n    base_url: http://localhost:4000/\n\
             \x20 - name: full\n    api: openai\n    base_url: http://localhost:4000/v1/chat/completions\n",
        );
        assert_eq!(t.by_name("slashed").unwrap().target_url(), "http://localhost:4000/v1/messages");
        assert_eq!(t.by_name("full").unwrap().target_url(), "http://localhost:4000/v1/chat/completions");
    }

    #[test]
    fn default_follows_the_flag_not_the_file_order() {
        let t = table(
            "providers:\n\
             \x20 - name: ollama\n    api: openai\n    base_url: http://localhost:11434/v1\n\
             \x20 - name: openai\n    api: openai\n    base_url: https://api.openai.com/v1\n    default: true\n",
        );
        assert_eq!(t.default_for(ApiStyle::OpenAI).name, "openai");
    }

    #[test]
    fn first_of_a_style_is_the_default_when_none_is_flagged() {
        let t = table(
            "providers:\n  - name: ollama\n    api: openai\n    base_url: http://localhost:11434/v1\n",
        );
        assert_eq!(t.default_for(ApiStyle::OpenAI).name, "ollama");
    }

    #[test]
    fn a_style_the_file_never_mentions_still_resolves_to_the_hosted_api() {
        // Only openai is configured, but an Anthropic-style client can still
        // be pointed at the proxy — it just reaches the hosted API.
        let t = table(
            "providers:\n  - name: ollama\n    api: openai\n    base_url: http://localhost:11434/v1\n",
        );
        let anthropic = t.default_for(ApiStyle::Anthropic);
        assert_eq!(anthropic.name, "anthropic");
        assert_eq!(anthropic.target_url(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn an_unparseable_file_falls_back_to_the_hosted_defaults() {
        let dir = std::env::temp_dir().join(format!("hw-providers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.yaml");
        std::fs::write(&path, "providers: [ this is not: valid: yaml").unwrap();
        let t = ProviderTable::load(&path);
        assert_eq!(t.default_for(ApiStyle::OpenAI).target_url(), "https://api.openai.com/v1/chat/completions");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lookup_by_name_is_case_insensitive_and_reports_unknown_names() {
        let t = table(
            "providers:\n  - name: Ollama\n    api: openai\n    base_url: http://localhost:11434/v1\n",
        );
        assert!(t.by_name("ollama").is_some());
        assert!(t.by_name("nope").is_none());
    }
}
