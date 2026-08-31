use serde::{Deserialize, Serialize};
use std::path::Path;

/// Which wire format an endpoint speaks. This is about *parsing* — how the
/// request and response bodies are shaped — and says nothing about who is
/// serving them: a local llama.cpp server speaking `openai` is handled by
/// exactly the same code path as api.openai.com.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub name: String,
    pub api: ApiStyle,
    /// What `providers.yaml` says — never overwritten in memory, so an env
    /// override never gets written back into the file behind the editor.
    pub base_url: String,
    /// The provider the bare `/v1/chat/completions` and `/v1/messages`
    /// routes resolve to, for clients that can set neither a path prefix
    /// nor a header. At most one per api style is meaningful; the first
    /// marked one wins.
    #[serde(default)]
    pub default: bool,
    /// Set when an env var replaces `base_url` for this entry: the name of
    /// that var, so the UI can say why the file's value isn't the one in
    /// effect. Neither read from nor written to the file.
    #[serde(skip)]
    pub env_override: Option<&'static str>,
    /// The base URL actually used, when an env var supplied it.
    #[serde(skip)]
    pub env_base_url: Option<String>,
}

impl ProviderConfig {
    /// The full URL a call to this provider is forwarded to. A `base_url`
    /// that already names the endpoint is left alone, so pasting a complete
    /// URL out of another tool's config also works.
    pub fn target_url(&self) -> String {
        let base = self.effective_base_url().trim_end_matches('/');
        let path = self.api.endpoint_path();
        if base.ends_with(path) {
            base.to_string()
        } else {
            format!("{base}{path}")
        }
    }
}

impl ProviderConfig {
    /// The base URL in effect: an env override if one applies, otherwise
    /// what the file says.
    pub fn effective_base_url(&self) -> &str {
        self.env_base_url.as_deref().unwrap_or(&self.base_url)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
        env_override: None,
        env_base_url: None,
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

    /// Builds a table from a list that has already been validated — the
    /// shape an edit arrives in, as opposed to `load`'s file.
    pub fn from_list(providers: Vec<ProviderConfig>) -> Self {
        Self::from_configs(providers)
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
            self.providers[index].env_override = Some(var);
            self.providers[index].env_base_url = Some(url.trim().to_string());
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

    /// Rejects a set of providers that would leave the proxy unable to route
    /// honestly: a nameless entry, two entries answering to the same name, a
    /// base URL that isn't one, or two defaults competing for the same style.
    /// Returned as a message meant to be shown to whoever is editing.
    pub fn validate(providers: &[ProviderConfig]) -> Result<(), String> {
        let mut seen: Vec<String> = Vec::new();
        for provider in providers {
            let name = provider.name.trim();
            if name.is_empty() {
                return Err("Every provider needs a name.".to_string());
            }
            if name.contains('/') || name.contains(char::is_whitespace) {
                return Err(format!(
                    "Provider name '{name}' can't contain spaces or slashes — it is used in the proxy path /p/<name>/…"
                ));
            }
            let lowered = name.to_ascii_lowercase();
            if seen.contains(&lowered) {
                return Err(format!("Two providers are both called '{name}'."));
            }
            seen.push(lowered);

            let url = provider.base_url.trim();
            if url.is_empty() {
                return Err(format!("Provider '{name}' has no base URL."));
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(format!(
                    "Provider '{name}' has base URL '{url}', which needs to start with http:// or https://"
                ));
            }
        }

        for style in [ApiStyle::OpenAI, ApiStyle::Anthropic] {
            let defaults = providers.iter().filter(|p| p.api == style && p.default).count();
            if defaults > 1 {
                return Err(format!(
                    "{} providers are marked as the default for the {} API; only one can be.",
                    defaults,
                    style.as_str()
                ));
            }
        }

        Ok(())
    }

    /// Writes the table back to `path`. Comments in the file are not
    /// preserved — a header saying so, and where the full documentation is,
    /// goes in their place.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let file = ProvidersFile { providers: self.providers.clone() };
        let body = serde_yaml::to_string(&file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let header = "\
# Providers Harnesswurm forwards to. Written by the desktop app's Settings
# tab, and equally editable by hand — but a save from the app rewrites this
# file, so comments added here do not survive one.
#
#   name      how the provider is addressed (/p/<name>/… or X-Provider), and
#             the name its calls are recorded under
#   api       wire format spoken: openai or anthropic
#   base_url  what the client would otherwise be pointed at: for openai the
#             part before /chat/completions, for anthropic the part before
#             /v1/messages
#   default   used by the bare /v1/chat/completions and /v1/messages routes
#
";
        std::fs::write(path, format!("{header}{body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(yaml: &str) -> ProviderTable {
        ProviderTable::parse(yaml).expect("test yaml parses")
    }

    fn config(name: &str, api: ApiStyle, base_url: &str, default: bool) -> ProviderConfig {
        ProviderConfig {
            name: name.to_string(),
            api,
            base_url: base_url.to_string(),
            default,
            env_override: None,
            env_base_url: None,
        }
    }

    #[test]
    fn validation_rejects_edits_that_could_not_be_routed() {
        let cases: Vec<(Vec<ProviderConfig>, &str)> = vec![
            (vec![config("", ApiStyle::OpenAI, "http://x/v1", false)], "name"),
            (vec![config("my provider", ApiStyle::OpenAI, "http://x/v1", false)], "spaces"),
            (
                vec![
                    config("ollama", ApiStyle::OpenAI, "http://x/v1", false),
                    config("Ollama", ApiStyle::OpenAI, "http://y/v1", false),
                ],
                "both called",
            ),
            (vec![config("ollama", ApiStyle::OpenAI, "", false)], "no base URL"),
            (vec![config("ollama", ApiStyle::OpenAI, "localhost:11434", false)], "http://"),
            (
                vec![
                    config("a", ApiStyle::OpenAI, "http://x/v1", true),
                    config("b", ApiStyle::OpenAI, "http://y/v1", true),
                ],
                "only one can be",
            ),
        ];
        for (providers, expected) in cases {
            let error = ProviderTable::validate(&providers).expect_err("should be rejected");
            assert!(error.contains(expected), "{error} should mention {expected}");
        }
    }

    #[test]
    fn validation_accepts_one_default_per_style() {
        let providers = vec![
            config("openai", ApiStyle::OpenAI, "https://api.openai.com/v1", true),
            config("ollama", ApiStyle::OpenAI, "http://localhost:11434/v1", false),
            config("anthropic", ApiStyle::Anthropic, "https://api.anthropic.com", true),
        ];
        assert!(ProviderTable::validate(&providers).is_ok());
    }

    #[test]
    fn a_saved_table_reloads_as_itself() {
        let dir = std::env::temp_dir().join(format!("hw-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.yaml");

        let table = ProviderTable::from_list(vec![
            config("ollama", ApiStyle::OpenAI, "http://localhost:11434/v1", true),
        ]);
        table.save(&path).unwrap();

        let reloaded = ProviderTable::load(&path);
        assert_eq!(reloaded.default_for(ApiStyle::OpenAI).name, "ollama");
        assert_eq!(
            reloaded.default_for(ApiStyle::OpenAI).target_url(),
            "http://localhost:11434/v1/chat/completions"
        );
        // The anthropic entry the table filled in for a style the list never
        // mentioned is saved too, so what is on disk is what is in effect.
        assert_eq!(reloaded.default_for(ApiStyle::Anthropic).name, "anthropic");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_env_override_changes_the_target_without_touching_the_saved_value() {
        let mut table = ProviderTable::from_list(vec![
            config("openai", ApiStyle::OpenAI, "https://api.openai.com/v1", true),
        ]);
        // Applied by hand rather than through the process env, which tests
        // running in parallel share.
        let entry = &mut table.providers[0];
        entry.env_override = Some("HARNESSWURM_OPENAI_BASE_URL");
        entry.env_base_url = Some("http://localhost:11434/v1".to_string());

        let openai = table.default_for(ApiStyle::OpenAI);
        assert_eq!(openai.target_url(), "http://localhost:11434/v1/chat/completions");
        // What an editor shows and saves stays the file's own value, so the
        // override can't quietly become permanent.
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
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
