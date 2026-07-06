use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    Ollama,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub provider: ProviderKind,
    pub base_url: String,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    #[serde(rename = "connection", default)]
    pub connections: Vec<Connection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML_FIXTURE: &str = r#"
[[connection]]
name = "local-vllm"
provider = "openai-compatible"
base_url = "http://localhost:8000/v1"
default_model = "qwen2.5-coder-32b"
models = ["qwen2.5-coder-32b", "llama-3.1-70b"]

[[connection]]
name = "home-ollama"
provider = "ollama"
base_url = "http://localhost:11434"
default_model = "llama3.1"
"#;

    #[test]
    fn parses_multiple_connections_from_toml() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        assert_eq!(file.connections.len(), 2);
        assert_eq!(file.connections[0].name, "local-vllm");
        assert_eq!(file.connections[0].provider, ProviderKind::OpenAiCompatible);
        assert_eq!(
            file.connections[0].models,
            vec!["qwen2.5-coder-32b", "llama-3.1-70b"]
        );
    }

    #[test]
    fn models_field_defaults_to_empty_when_omitted() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        assert_eq!(file.connections[1].name, "home-ollama");
        assert_eq!(file.connections[1].provider, ProviderKind::Ollama);
        assert!(file.connections[1].models.is_empty());
    }

    #[test]
    fn round_trips_through_serialization() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        let serialized = toml::to_string(&file).expect("serializes");
        let reparsed: ConnectionsFile = toml::from_str(&serialized).expect("reparses");
        assert_eq!(file, reparsed);
    }
}
