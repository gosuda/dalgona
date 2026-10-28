//! The host and model facts a Chat Completions body depends on.
//!
//! The shared codec writes the reference body. A few hosts take it with
//! published differences: the member a requested output cap travels in,
//! whether the stream options are asked for, how thinking is switched, whether
//! assistant reasoning travels back, which JSON Schema subset a tool
//! declaration may use, whether tools carry a `strict` flag and a
//! `tool_stream` switch, and whether `prompt_cache_key` is sent at all.
//! [`Dialect::of`] derives them from the provider row id, its base URL, and
//! the provider-native model id; every other triple is [`Dialect::STANDARD`].
//! A dialect is immutable data of that triple; no configuration key selects
//! it, and [`Kind::of`] is its only classifier.

use dal_core::ThinkingLevel;
use serde::Serialize;

use super::WireCall;
use crate::{family::schema::ToolSchemas, thinking::ThinkingSupport};

/// The body member a requested output cap travels in. Every dialect sends a
/// requested cap; only the member differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputCap {
    /// `max_completion_tokens`, the reference field.
    CompletionTokens,
    /// `max_tokens`, for hosts that publish only the legacy field.
    Tokens,
}

impl OutputCap {
    /// The `(max_completion_tokens, max_tokens)` members for a requested cap.
    pub(super) fn lower(self, cap: Option<u32>) -> (Option<u32>, Option<u32>) {
        match self {
            Self::CompletionTokens => (cap, None),
            Self::Tokens => (None, cap),
        }
    }
}

/// Whether the body asks for `stream_options.include_usage`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamUsage {
    /// The body carries `stream_options.include_usage`.
    Requested,
    /// The host's request schema has no `stream_options`; its stream still
    /// reports `usage` on the final chunk.
    Omitted,
}

/// The `thinking` member of a host with a thinking switch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Switch {
    Enabled,
    Disabled,
    /// Z.AI's enabled spelling, which carries `clear_thinking: false`.
    ZaiEnabled,
}

impl Serialize for Switch {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let zai = matches!(self, Self::ZaiEnabled);
        let mut map = serializer.serialize_map(Some(if zai { 2 } else { 1 }))?;
        map.serialize_entry(
            "type",
            match self {
                Self::Enabled | Self::ZaiEnabled => "enabled",
                Self::Disabled => "disabled",
            },
        )?;
        if zai {
            map.serialize_entry("clear_thinking", &false)?;
        }
        map.end()
    }
}

/// Whether the body asks a host with a tool-stream switch for one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolStream {
    /// The host's request schema has no `tool_stream`.
    Omitted,
    /// `tool_stream: true` beside the tools.
    Sent,
}

/// Whether the body carries `prompt_cache_key`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptCacheKey {
    /// `prompt_cache_key` from the request's cache key.
    Sent,
    /// The host's request schema has no `prompt_cache_key`; its cache key
    /// travels in a header the request path owns, if anywhere.
    Omitted,
}

/// How the thinking plan becomes request members.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThinkingMembers {
    /// `reasoning_effort` as the plan spells it.
    Effort,
    /// A toggleable `thinking` object and no effort: `enabled` for any on
    /// level, `disabled` for level off, nothing when the plan sends nothing
    /// (a model whose ladder has no off).
    Switch,
    /// An always-on `thinking` object with the effort: both at an on level,
    /// neither at level off. `disabled` and `none` are never written.
    AlwaysOnWithEffort,
    /// A qwen-format row: `enable_thinking` spells the plan's presence and
    /// no effort member exists; the member is written at every level.
    QwenToggle,
    /// A deepseek-format row: `thinking` is `enabled` with an effort and
    /// `disabled` without one, and no effort member exists.
    DeepseekToggle,
    /// A Z.AI row: `thinking` is `{enabled, clear_thinking:false}` with an
    /// effort and `disabled` without one; `with_effort` rows also spell the
    /// effort beside the switch.
    ZaiToggle { with_effort: bool },
    /// Z.AI `glm-5.3*`: always the enabled spelling, with the effort beside
    /// it when the plan spells one. The model cannot be switched off.
    ZaiAlwaysOn,
    /// A row whose source sends no thinking member at any level.
    Never,
}

/// The thinking members one plan lowers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Lowered {
    pub(super) effort: Option<&'static str>,
    pub(super) thinking: Option<Switch>,
    pub(super) enable_thinking: Option<bool>,
}

impl ThinkingMembers {
    /// The thinking members of a plan whose effective `level` spells
    /// `effort`.
    pub(super) fn lower(self, effort: Option<&'static str>, level: ThinkingLevel) -> Lowered {
        match self {
            Self::Effort => Lowered {
                effort,
                thinking: None,
                enable_thinking: None,
            },
            Self::Switch => Lowered {
                effort: None,
                thinking: effort.map(|_| {
                    if level == ThinkingLevel::Off {
                        Switch::Disabled
                    } else {
                        Switch::Enabled
                    }
                }),
                enable_thinking: None,
            },
            Self::AlwaysOnWithEffort => match effort {
                Some(effort) if level != ThinkingLevel::Off => Lowered {
                    effort: Some(effort),
                    thinking: Some(Switch::Enabled),
                    enable_thinking: None,
                },
                _ => Lowered {
                    effort: None,
                    thinking: None,
                    enable_thinking: None,
                },
            },
            Self::QwenToggle => Lowered {
                effort: None,
                thinking: None,
                enable_thinking: Some(level != ThinkingLevel::Off),
            },
            Self::DeepseekToggle => Lowered {
                effort: None,
                thinking: Some(if level == ThinkingLevel::Off {
                    Switch::Disabled
                } else {
                    Switch::Enabled
                }),
                enable_thinking: None,
            },
            Self::ZaiToggle { with_effort } => Lowered {
                effort: if with_effort && level != ThinkingLevel::Off {
                    effort
                } else {
                    None
                },
                thinking: Some(if level == ThinkingLevel::Off {
                    Switch::Disabled
                } else {
                    Switch::ZaiEnabled
                }),
                enable_thinking: None,
            },
            Self::ZaiAlwaysOn => Lowered {
                effort: if level == ThinkingLevel::Off {
                    None
                } else {
                    effort
                },
                thinking: Some(Switch::ZaiEnabled),
                enable_thinking: None,
            },
            Self::Never => Lowered {
                effort: None,
                thinking: None,
                enable_thinking: None,
            },
        }
    }
}

/// What travels back of an assistant message's reasoning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReasoningReplay {
    /// Reasoning parts never travel.
    Dropped,
    /// The model's own reasoning travels as `reasoning_content`; a message
    /// without it carries no member.
    OwnWhenPresent,
    /// The model's own reasoning travels as `reasoning_content`, and a message
    /// with tool calls carries the member empty when there is none of this
    /// model's to send. Text-only messages without reasoning carry none.
    OwnOrEmptyOnCalls,
    /// The host requires the member on every replayed assistant message: the
    /// model's own reasoning when the message carries any, an empty member
    /// otherwise.
    OwnOrEmptyEveryMessage,
}

impl ReasoningReplay {
    /// The `reasoning_content` of one assistant message: `own` are the
    /// non-blank reasoning parts this model wrote (none for a message of
    /// another model), `calls` its tool calls.
    pub(super) fn content(self, own: &[&str], calls: &[WireCall<'_>]) -> Option<String> {
        let own = (!own.is_empty()).then(|| own.join("\n"));
        match self {
            Self::Dropped => None,
            Self::OwnWhenPresent => own,
            Self::OwnOrEmptyOnCalls => own.or_else(|| (!calls.is_empty()).then(String::new)),
            Self::OwnOrEmptyEveryMessage => own.or_else(|| Some(String::new())),
        }
    }
}

/// The host-specific facts of one Chat request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Dialect {
    pub(crate) output_cap: OutputCap,
    pub(crate) stream_usage: StreamUsage,
    pub(crate) thinking: ThinkingMembers,
    pub(crate) replay: ReasoningReplay,
    pub(crate) schemas: ToolSchemas,
    pub(crate) tool_stream: ToolStream,
    /// The per-tool `strict` member the source writes beside `parameters`
    /// (`strict: false` for every tool of these rows).
    pub(crate) strict_tools: bool,
    pub(crate) prompt_cache: PromptCacheKey,
}

impl Dialect {
    /// The reference body: `max_completion_tokens`, stream options,
    /// `reasoning_effort`, `prompt_cache_key`, no reasoning replay, schemas
    /// verbatim, no tool flags.
    pub(crate) const STANDARD: Self = Self {
        output_cap: OutputCap::CompletionTokens,
        stream_usage: StreamUsage::Requested,
        thinking: ThinkingMembers::Effort,
        replay: ReasoningReplay::Dropped,
        schemas: ToolSchemas::Verbatim,
        tool_stream: ToolStream::Omitted,
        strict_tools: false,
        prompt_cache: PromptCacheKey::Sent,
    };

    /// The reference body for a host that publishes only `max_tokens`.
    pub(crate) const LEGACY_TOKENS: Self = Self {
        output_cap: OutputCap::Tokens,
        ..Self::STANDARD
    };

    /// The dialect of model `model` on the provider row `provider` served at
    /// `base_url`.
    #[must_use]
    pub(crate) fn of(provider: &str, base_url: &str, model: &str) -> Self {
        let mut dialect = Kind::of(provider, base_url, model).dialect();
        if requires_replay(provider, model) {
            dialect.replay = ReasoningReplay::OwnOrEmptyEveryMessage;
        }
        dialect
    }

    /// The thinking ladder the host publishes for `model`, when it is not the
    /// family default.
    #[must_use]
    pub(crate) fn thinking_support(provider: &str, model: &str) -> Option<ThinkingSupport> {
        Kind::of(provider, "", model).ladder()
    }
}

/// Rows whose source fills `reasoning_content` on every replayed assistant
/// message (`requiresReasoningContentOnAssistantMessages`): the model's own
/// reasoning, or an empty member when the message carries none.
fn requires_replay(provider: &str, model: &str) -> bool {
    match provider {
        "alibaba-token-plan" => is_series(model, "deepseek-v3") || is_series(model, "deepseek-v4"),
        "opencode" => is_series(model, "deepseek-v4"),
        "opencode-go" => {
            is_series(model, "deepseek-v4-pro") || is_series(model, "deepseek-v4.1-flash")
        }
        "cloudflare-ai-gateway" | "cloudflare-workers-ai" => {
            model.contains("@cf/deepseek-ai/deepseek-v4")
        }
        _ => false,
    }
}

/// Base URL fragments of the hosts that publish only `max_tokens`.
const LEGACY_TOKEN_URLS: [&str; 10] = [
    "chutes.ai",
    "deepseek.com",
    "api.moonshot.",
    "gateway.ai.cloudflare.com",
    "api.together.ai",
    "api.together.xyz",
    "integrate.api.nvidia.com",
    "api.ant-ling.com",
    "api.z.ai",
    "open.bigmodel.cn",
];

/// The hosts and models with a published difference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Standard,
    /// A host whose published cap field is `max_tokens`.
    LegacyTokens,
    /// Any other Cerebras model.
    Cerebras,
    /// `gpt-oss-*` on Cerebras.
    CerebrasGptOss,
    /// Any other Moonshot model.
    Moonshot,
    /// `kimi-k3*`: always reasons, takes an effort.
    KimiK3,
    /// `kimi-k2.6*`: a thinking toggle.
    KimiK26,
    /// `kimi-k2.7-code*`: thinking is always enabled.
    KimiK27Code,
    /// `mimo-v2.5*`: always on, with its effort.
    MimoV25,
    /// `mimo-v2.6*`: a toggle, no effort.
    MimoV26,
    /// A Bai completions row: strict `false` on every tool, rewritten
    /// schemas, no thinking member, no `prompt_cache_key`.
    Bai,
    /// A Z.AI row whose `thinking` switch follows the plan's effort.
    ZaiToggle {
        with_effort: bool,
    },
    /// Z.AI `glm-5.3*`: always the enabled switch.
    ZaiAlwaysOn,
    /// A qwen-format row of the token plan.
    QwenToggle,
    /// A deepseek-format row: the `thinking` switch, never an effort.
    DeepseekToggle,
    /// A row whose source sends no thinking member at all.
    NoThinking,
}

impl Kind {
    fn of(provider: &str, base_url: &str, model: &str) -> Self {
        match provider {
            "cerebras" if model.starts_with("gpt-oss") => Self::CerebrasGptOss,
            "cerebras" => Self::Cerebras,
            "moonshot" | "moonshotai-cn" => Self::moonshot(model),
            "xiaomi"
            | "xiaomi-token-plan-ams"
            | "xiaomi-token-plan-cn"
            | "xiaomi-token-plan-sgp" => Self::mimo(model),
            "alibaba-token-plan" => Self::alibaba(model),
            "zai" | "zai-coding-cn" => Self::zai(model),
            "opencode" | "opencode-go" => Self::opencode(provider, model),
            "bai" => Self::Bai,
            "cloudflare-ai-gateway" | "cloudflare-workers-ai" => Self::cloudflare(model),
            "deepseek"
            | "together"
            | "nvidia"
            | "ant-ling"
            | "zhipu-coding-plan"
            | "baseten"
            | "charm-hyper"
            | "coralbricks"
            | "singularityapi-dev"
            | "singularityapi-tech"
            | "stepfun" => Self::LegacyTokens,
            _ if LEGACY_TOKEN_URLS.iter().any(|host| base_url.contains(host)) => Self::LegacyTokens,
            _ => Self::Standard,
        }
    }

    /// The id series of an Alibaba Token Plan row: `qwen`-format rows toggle
    /// `enable_thinking`, `deepseek`- and `kimi`-format rows the `thinking`
    /// switch, and every other row (the `glm` and `MiniMax` ids) has no switch.
    fn alibaba(model: &str) -> Self {
        if model.starts_with("qwen3.") {
            Self::QwenToggle
        } else if is_series(model, "deepseek-v3")
            || is_series(model, "deepseek-v4")
            || is_series(model, "kimi-k2.5")
            || is_series(model, "kimi-k2.6")
            || is_series(model, "kimi-k2.7-code")
        {
            Self::DeepseekToggle
        } else {
            Self::NoThinking
        }
    }

    /// The id series of a Z.AI row: `glm-5.3*` always reasons; `glm-5.2*`
    /// also spells the effort; the rest toggle only.
    fn zai(model: &str) -> Self {
        if is_series(model, "glm-5.3") {
            Self::ZaiAlwaysOn
        } else if is_series(model, "glm-5.2") {
            Self::ZaiToggle { with_effort: true }
        } else {
            Self::ZaiToggle { with_effort: false }
        }
    }

    /// The id series of an `OpenCode` row: only the Go lane's `deepseek` ids and
    /// Zen's `kimi-k2.6` carry a switch; every other completions id has none.
    fn opencode(provider: &str, model: &str) -> Self {
        let deepseek_format = provider == "opencode-go"
            && (is_series(model, "deepseek-v4-pro") || is_series(model, "deepseek-v4.1-flash"));
        let kimi_toggle = provider == "opencode" && is_series(model, "kimi-k2.6");
        if deepseek_format || kimi_toggle {
            Self::DeepseekToggle
        } else {
            Self::NoThinking
        }
    }

    /// The id series of a Cloudflare completions row: only the two Workers AI
    /// deepseek ids carry a switch.
    fn cloudflare(model: &str) -> Self {
        if model.contains("@cf/deepseek-ai/deepseek-v4") {
            Self::DeepseekToggle
        } else {
            Self::NoThinking
        }
    }

    fn moonshot(model: &str) -> Self {
        if is_series(model, "kimi-k3") {
            Self::KimiK3
        } else if is_series(model, "kimi-k2.6") {
            Self::KimiK26
        } else if is_series(model, "kimi-k2.7-code") {
            Self::KimiK27Code
        } else {
            Self::Moonshot
        }
    }

    /// The id series of a Xiaomi row; an id no source describes is standard.
    fn mimo(model: &str) -> Self {
        if is_series(model, "mimo-v2.5") {
            Self::MimoV25
        } else if is_series(model, "mimo-v2.6") {
            Self::MimoV26
        } else {
            Self::Standard
        }
    }

    fn dialect(self) -> Dialect {
        const CEREBRAS: Dialect = Dialect {
            stream_usage: StreamUsage::Omitted,
            ..Dialect::STANDARD
        };
        const MOONSHOT: Dialect = Dialect {
            output_cap: OutputCap::Tokens,
            schemas: ToolSchemas::Moonshot,
            ..Dialect::STANDARD
        };
        // The S8 completions rows: the source rewrites every tool schema to
        // its OpenAI-compatible subset, writes `strict: false` on every tool,
        // and never sends `prompt_cache_key`.
        const S8_COMPAT: Dialect = Dialect {
            schemas: ToolSchemas::OpenAiCompat,
            strict_tools: true,
            prompt_cache: PromptCacheKey::Omitted,
            ..Dialect::STANDARD
        };
        // The same body where the source caps the output with `max_tokens`.
        const S8_COMPAT_TOKENS: Dialect = Dialect {
            output_cap: OutputCap::Tokens,
            ..S8_COMPAT
        };
        match self {
            Self::Standard => Dialect::STANDARD,
            Self::LegacyTokens => Dialect::LEGACY_TOKENS,
            Self::Cerebras | Self::CerebrasGptOss => CEREBRAS,
            Self::Moonshot => MOONSHOT,
            Self::KimiK3 => Dialect {
                replay: ReasoningReplay::OwnOrEmptyOnCalls,
                ..MOONSHOT
            },
            Self::KimiK26 | Self::KimiK27Code => Dialect {
                thinking: ThinkingMembers::Switch,
                replay: ReasoningReplay::OwnWhenPresent,
                ..MOONSHOT
            },
            Self::MimoV25 => Dialect {
                thinking: ThinkingMembers::AlwaysOnWithEffort,
                replay: ReasoningReplay::OwnOrEmptyOnCalls,
                ..Dialect::STANDARD
            },
            Self::MimoV26 => Dialect {
                thinking: ThinkingMembers::Switch,
                replay: ReasoningReplay::OwnOrEmptyOnCalls,
                ..Dialect::STANDARD
            },
            Self::Bai => Dialect {
                thinking: ThinkingMembers::Never,
                ..S8_COMPAT
            },
            Self::NoThinking => Dialect {
                thinking: ThinkingMembers::Never,
                ..S8_COMPAT_TOKENS
            },
            Self::QwenToggle => Dialect {
                thinking: ThinkingMembers::QwenToggle,
                ..S8_COMPAT_TOKENS
            },
            Self::DeepseekToggle => Dialect {
                thinking: ThinkingMembers::DeepseekToggle,
                ..S8_COMPAT_TOKENS
            },
            Self::ZaiToggle { with_effort } => Dialect {
                thinking: ThinkingMembers::ZaiToggle { with_effort },
                tool_stream: ToolStream::Sent,
                ..S8_COMPAT_TOKENS
            },
            Self::ZaiAlwaysOn => Dialect {
                thinking: ThinkingMembers::ZaiAlwaysOn,
                tool_stream: ToolStream::Sent,
                ..S8_COMPAT_TOKENS
            },
        }
    }

    fn ladder(self) -> Option<ThinkingSupport> {
        use ThinkingLevel::{High, Low, Max, Medium, Minimal, Xhigh};
        let (accepted, none_supported) = match self {
            // `low`, `medium`, `high`; "Disable reasoning: Not supported".
            Self::CerebrasGptOss => (vec![Low, Medium, High], false),
            // `low`, `high`, `max`; always reasons.
            Self::KimiK3 => (vec![Low, High, Max], false),
            // One on level and a `disabled` switch.
            Self::KimiK26 => (vec![High], true),
            // Always enabled; every on level spells the same switch, including
            // the always-on Z.AI rows, which cannot be switched off.
            Self::KimiK27Code | Self::ZaiAlwaysOn => {
                (vec![Minimal, Low, Medium, High, Xhigh, Max], false)
            }
            // The pinned efforts; the rows cannot be switched off.
            Self::MimoV25 => (vec![Minimal, Low, Medium, High], false),
            // The S8 switch rows: every on level spells the same toggle; only
            // the always-on Z.AI rows cannot be switched off.
            Self::QwenToggle | Self::DeepseekToggle | Self::ZaiToggle { .. } => {
                (vec![Minimal, Low, Medium, High, Xhigh, Max], true)
            }
            // These rows' sources send no thinking member and publish no
            // ladder; an empty accepted set keeps every level off.
            Self::Bai | Self::NoThinking => (Vec::new(), false),
            // `mimo-v2.6*` keeps the family default: every level, off allowed.
            Self::Standard
            | Self::LegacyTokens
            | Self::Cerebras
            | Self::Moonshot
            | Self::MimoV26 => return None,
        };
        Some(ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        })
    }
}

/// Whether `model` names `series` or a revision of it (`kimi-k3`,
/// `kimi-k3.1-pro`, `mimo-v2.5-pro`, `kimi-k2.7-code-highspeed`).
fn is_series(model: &str, series: &str) -> bool {
    model
        .strip_prefix(series)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '-', '_', ':', '/']))
}
