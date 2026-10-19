//! The models the app knows (docs/ai-agent-plan.md §4): context length and
//! whether the model takes function tools. The context lengths are the
//! `context_length` of Fireworks' own `/inference/v1/models` list (P0,
//! 2026-10-19).

/// The default: the model the maintainer's OpenCode setup uses. P0 confirmed
/// it is served and calls tools.
pub const DEFAULT_MODEL: &str = "accounts/fireworks/models/deepseek-v4p1-flash";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// Tokens.
    pub context: u64,
    pub tools: bool,
}

pub const PRESETS: &[ModelInfo] = &[
    ModelInfo {
        id: DEFAULT_MODEL,
        name: "DeepSeek V4.1 Flash",
        context: 1_048_576,
        tools: true,
    },
    ModelInfo {
        id: "accounts/fireworks/models/glm-5p3-flash",
        name: "GLM 5.3 Flash",
        context: 1_048_576,
        tools: true,
    },
    ModelInfo {
        id: "accounts/fireworks/models/kimi-k3",
        name: "Kimi K3",
        context: 1_048_576,
        tools: true,
    },
    ModelInfo {
        id: "accounts/fireworks/models/minimax-m3",
        name: "MiniMax M3",
        context: 512_000,
        tools: true,
    },
    ModelInfo {
        id: "accounts/fireworks/models/gpt-oss-120b",
        name: "gpt-oss 120B",
        context: 131_072,
        tools: true,
    },
];

/// Context of a model id the table does not list.
pub const UNKNOWN_CONTEXT: u64 = 32_768;

/// What is known of `id`; an unknown id is taken as 32k context with tools
/// (a refused `tools` field then turns them off with a notice).
pub fn info(id: &str) -> ModelInfo {
    PRESETS
        .iter()
        .find(|m| m.id == id)
        .copied()
        .unwrap_or(ModelInfo {
            id: "",
            name: "",
            context: UNKNOWN_CONTEXT,
            tools: true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_a_preset_and_unknown_ids_are_cautious() {
        assert_eq!(info(DEFAULT_MODEL).context, 1_048_576);
        assert!(PRESETS.iter().any(|m| m.id == DEFAULT_MODEL));
        let unknown = info("accounts/x/models/new");
        assert_eq!(unknown.context, UNKNOWN_CONTEXT);
        assert!(unknown.tools);
    }
}
