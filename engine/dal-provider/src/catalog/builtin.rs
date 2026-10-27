//! Built-in model rows used by the provider catalog.
//!
//! Keep each row's source notes aligned with its values when updating the catalog.

use dal_core::ThinkingLevel;

use super::{CatalogEntry, ImageProfile, Listing, ToolSupport};
use crate::{
    provider::{OPENAI, OPENAI_CODEX},
    thinking::ThinkingSupport,
    usage::{LUNA_RESERVE_DISPLAY, LUNA_RESERVE_MODEL},
};

/// Returns the built-in model rows.
#[must_use]
pub fn built_in_entries() -> Vec<CatalogEntry> {
    use ThinkingLevel::{High, Low, Max, Medium, Xhigh};

    // `prices/models.dev.json` marks each built-in OpenAI row below as not
    // supporting temperature.
    let levels = vec![Low, Medium, High, Xhigh, Max];
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-astra
    // states 922,000 maximum input and 128,000 maximum output tokens.
    // The model page lists Responses tools.
    // This row uses the Responses API.
    let astra = openai_entry(
        "gpt-6-astra",
        "GPT-6 Astra",
        922_000,
        128_000,
        levels.clone(),
        false,
        ToolSupport::ResponsesOnly,
    );
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-sol
    // states 922,000 maximum input and 128,000 maximum output tokens. Chat
    // function calling requires reasoning_effort `none` on this model.
    let sol = openai_entry(
        "gpt-6-sol",
        "GPT-6 Sol",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::ChatWhenNoReasoning,
    );
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-6-luna
    // states 922,000 maximum input and 128,000 maximum output tokens. Chat
    // function calling requires reasoning_effort `none` on this model.
    let luna = openai_entry(
        "gpt-6-luna",
        "GPT-6 Luna",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::ChatWhenNoReasoning,
    );
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-sol
    // documents `gpt-5.6` as its alias, with 922,000 input and 128,000 output.
    let gpt_56 = openai_entry(
        "gpt-5.6",
        "GPT-5.6",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    // Chat Completions, Responses, and function calling are supported.
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-sol
    let gpt_56_sol = openai_entry(
        "gpt-5.6-sol",
        "GPT-5.6 Sol",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    // Chat Completions, Responses, and function calling are supported.
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-luna
    let gpt_56_luna = openai_entry(
        "gpt-5.6-luna",
        "GPT-5.6 Luna",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    // Chat Completions, Responses, and function calling are supported.
    // Verified 2026-09-28: https://developers.openai.com/api/docs/models/gpt-5.6-terra
    let gpt_56_terra = openai_entry(
        "gpt-5.6-terra",
        "GPT-5.6 Terra",
        922_000,
        128_000,
        levels.clone(),
        true,
        ToolSupport::Any,
    );
    let mut entries = vec![
        astra,
        sol,
        luna,
        gpt_56,
        gpt_56_sol,
        gpt_56_luna,
        gpt_56_terra,
        codex_reserve_entry(levels),
    ];
    entries.extend(kimi_coding_seeds());
    entries.extend(zai_seeds());
    entries.extend(zai_coding_cn_seeds());
    entries.extend(alibaba_token_plan_seeds());
    entries.extend(opencode_seeds());
    entries.extend(opencode_go_seeds());
    entries.extend(vercel_ai_gateway_seeds());
    entries
}

/// Builds the hidden Codex reserve row.
///
/// The reserve input window is 95% of 272,000 tokens (258,400), and no output
/// cap is set. Remote compaction and reasoning summaries remain enabled.
fn codex_reserve_entry(accepted: Vec<ThinkingLevel>) -> CatalogEntry {
    CatalogEntry {
        provider: OPENAI_CODEX.id.into(),
        id: LUNA_RESERVE_MODEL.into(),
        display: LUNA_RESERVE_DISPLAY.into(),
        listing: Listing::Hidden,
        context_window: Some(258_400),
        max_output: None,
        thinking: ThinkingSupport::OpenAi {
            accepted,
            none_supported: false,
        },
        image_input: true,
        image_profile: None,
        remote_compact: true,
        supports_reasoning_summaries: true,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
        premium_multiplier: None,
        price: None,
    }
}

/// Builds a listed `OpenAI` catalog row with remote compaction enabled.
fn openai_entry(
    id: &str,
    display: &str,
    context_window: u32,
    max_output: u32,
    accepted: Vec<ThinkingLevel>,
    none_supported: bool,
    tool_support: ToolSupport,
) -> CatalogEntry {
    CatalogEntry {
        provider: OPENAI.id.into(),
        id: id.into(),
        display: display.into(),
        listing: Listing::Listed,
        context_window: Some(context_window),
        max_output: Some(max_output),
        thinking: ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        },
        image_input: true,
        image_profile: Some(ImageProfile::openai()),
        remote_compact: true,
        supports_reasoning_summaries: false,
        tool_support,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
        premium_multiplier: None,
        price: None,
    }
}

// Authored vendor seed rows (wire contract section 4, slice 12). Ids are
// model identifiers verified against the vendor documentation cited per
// group; every capability a vendor does not publish stays `None` or `false`
// and no number is invented. Chat body compat quirks (thinking formats,
// token fields, reasoning replay) belong to the one chat dialect table,
// never to these rows. Rows key on the logical provider id the wire owner
// binds, and a live row of the same `(provider, id)` replaces its seed via
// `Catalog::with_sources`.

const KIMI_CODING: &str = "kimi-coding";
const ZAI: &str = "zai";
const ZAI_CODING_CN: &str = "zai-coding-cn";
const ALIBABA_TOKEN_PLAN: &str = "alibaba-token-plan";
const OPENCODE: &str = "opencode";
const OPENCODE_GO: &str = "opencode-go";
const VERCEL_AI_GATEWAY: &str = "vercel-ai-gateway";

/// Builds one authored vendor catalog row.
///
/// `display` is the id (the typed-fallback convention) and every field a
/// vendor page has not verified keeps its honest default: `None` numbers,
/// no image input, no temperature, no price, no premium multiplier.
fn seed_entry(
    provider: &str,
    id: &str,
    thinking: &ThinkingSupport,
    context_window: Option<u32>,
    max_output: Option<u32>,
    image_input: bool,
) -> CatalogEntry {
    CatalogEntry {
        provider: provider.into(),
        id: id.into(),
        display: id.into(),
        listing: Listing::Listed,
        context_window,
        max_output,
        thinking: thinking.clone(),
        image_input,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
        premium_multiplier: None,
        price: None,
    }
}

/// One authored row per id: listed, family-honest thinking ladder, and no
/// stated numbers.
fn grouped_seeds(provider: &str, ids: &[&str], thinking: &ThinkingSupport) -> Vec<CatalogEntry> {
    ids.iter()
        .map(|id| seed_entry(provider, id, thinking, None, None, false))
        .collect()
}

/// The full `OpenAI` effort ladder the typed fallback grants Chat and
/// Responses rows; seeds mirror it so a seeded id and a typed id of the
/// same family clamp identically.
fn chat_ladder() -> ThinkingSupport {
    ThinkingSupport::OpenAi {
        accepted: vec![
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
            ThinkingLevel::Max,
        ],
        none_supported: true,
    }
}

/// The empty effort ladder the typed fallback grants Google rows; it sends
/// no reasoning control instead of one the model may reject.
fn google_ladder() -> ThinkingSupport {
    ThinkingSupport::OpenAi {
        accepted: Vec::new(),
        none_supported: true,
    }
}

/// Kimi for Coding: Anthropic-compatible endpoint `https://api.kimi.com/coding/`
/// (Kimi Code Docs, third-party tools).
///
/// Model table verified 2026-10-26,
/// <https://www.kimi.com/code/docs/en/kimi-code/models>: context windows
/// `k3`/`kimi-for-coding` 1,048,576 and
/// `k3-256k`/`kimi-for-coding-highspeed` 262,144, image input on all four.
/// Output caps are not published and stay `None`. `kimi-k2-thinking`
/// remains a fork catalog fact the current vendor table no longer lists,
/// so it states nothing.
fn kimi_coding_seeds() -> Vec<CatalogEntry> {
    let thinking = ThinkingSupport::UnknownAdaptive;
    vec![
        seed_entry(KIMI_CODING, "k3", &thinking, Some(1_048_576), None, true),
        seed_entry(KIMI_CODING, "k3-256k", &thinking, Some(262_144), None, true),
        seed_entry(
            KIMI_CODING,
            "kimi-for-coding",
            &thinking,
            Some(1_048_576),
            None,
            true,
        ),
        seed_entry(
            KIMI_CODING,
            "kimi-for-coding-highspeed",
            &thinking,
            Some(262_144),
            None,
            true,
        ),
        seed_entry(
            KIMI_CODING,
            "kimi-k2-thinking",
            &thinking,
            None,
            None,
            false,
        ),
    ]
}

/// Z.AI coding-plan rows at `https://api.z.ai/api/coding/paas/v4`.
///
/// Vendor model pages verified 2026-10-26: GLM-5.3
/// (<https://docs.z.ai/guides/llm/glm-5.3>) states a 1M-token context
/// window, 128K maximum output, and text-only input; GLM-5.2
/// (<https://docs.z.ai/guides/llm/glm-5.2>) states 1M context and 128K
/// maximum output, text-only input; GLM-5.3-Flash
/// (<https://docs.z.ai/guides/vlm/glm-5.3-flash>) states 1M context, 128K
/// maximum output, and image/video input. `1M`/`128K` are authored as
/// 1,048,576/131,072. The remaining ids have no vendor model page in the
/// current documentation index and state nothing.
fn zai_seeds() -> Vec<CatalogEntry> {
    let thinking = chat_ladder();
    vec![
        seed_entry(ZAI, "glm-4.7", &thinking, None, None, false),
        seed_entry(ZAI, "glm-5-turbo", &thinking, None, None, false),
        seed_entry(
            ZAI,
            "glm-5.2",
            &thinking,
            Some(1_048_576),
            Some(131_072),
            false,
        ),
        seed_entry(ZAI, "glm-5.2-highspeed", &thinking, None, None, false),
        seed_entry(
            ZAI,
            "glm-5.3",
            &thinking,
            Some(1_048_576),
            Some(131_072),
            false,
        ),
        seed_entry(
            ZAI,
            "glm-5.3-flash",
            &thinking,
            Some(1_048_576),
            Some(131_072),
            true,
        ),
        seed_entry(ZAI, "glm-5.3-highspeed", &thinking, None, None, false),
    ]
}

/// Z.AI CN coding-plan rows at `https://open.bigmodel.cn/api/coding/paas/v4`
/// (the same model catalog as the `zhipu-coding-plan` endpoint, distinct id
/// and key).
///
/// GLM-5.3 and GLM-5.3-Flash carry the vendor model facts cited in
/// [`zai_seeds`]. GLM-4.6V is the vendor's vision-language model with a
/// 128K-token context (verified 2026-10-26,
/// <https://www.zhipuai.cn/en/glm46v>). `glm-5.3-highspeed` has no vendor
/// model page and states nothing.
fn zai_coding_cn_seeds() -> Vec<CatalogEntry> {
    let thinking = chat_ladder();
    vec![
        seed_entry(
            ZAI_CODING_CN,
            "glm-4.6v",
            &thinking,
            Some(131_072),
            None,
            true,
        ),
        seed_entry(
            ZAI_CODING_CN,
            "glm-5.3",
            &thinking,
            Some(1_048_576),
            Some(131_072),
            false,
        ),
        seed_entry(
            ZAI_CODING_CN,
            "glm-5.3-flash",
            &thinking,
            Some(1_048_576),
            Some(131_072),
            true,
        ),
        seed_entry(
            ZAI_CODING_CN,
            "glm-5.3-highspeed",
            &thinking,
            None,
            None,
            false,
        ),
    ]
}

/// Alibaba Token Plan rows at
/// `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`
/// (OpenAI-compatible; Alibaba Cloud Model Studio token plan quick start,
/// <https://www.alibabacloud.com/help/en/model-studio/token-plan-quickstart>).
///
/// The hosted catalog spans several model families and the vendor page
/// lists ids without per-model token limits dal could state, so every row
/// carries `None` numbers and no capability beyond the family ladder.
fn alibaba_token_plan_seeds() -> Vec<CatalogEntry> {
    let thinking = chat_ladder();
    let ids = [
        "deepseek-v3.2",
        "deepseek-v4.1-flash",
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
        "deepseek-v4-pro-0813",
        "glm-5",
        "glm-5.1",
        "glm-5.2",
        "glm-5.3",
        "kimi-k2.5",
        "kimi-k2.6",
        "kimi-k2.7-code",
        "MiniMax-M2.5",
        "qwen3.6-flash",
        "qwen3.6-plus",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.8-flash",
        "qwen3.8-max",
        "qwen3.8-max-preview",
    ];
    ids.iter()
        .map(|id| seed_entry(ALIBABA_TOKEN_PLAN, id, &thinking, None, None, false))
        .collect()
}

/// `OpenCode` Zen rows. Anthropic-protocol ids ride `https://opencode.ai/zen`
/// and the rest `https://opencode.ai/zen/v1` (Zen docs,
/// <https://opencode.ai/docs/zen>). The operator's directory states no
/// per-model token limits dal could author, so numbers stay `None`. The
/// classifier ids (`jev-1.13`, `jev-1.13-free`) stay census records and are
/// not seeded.
fn opencode_seeds() -> Vec<CatalogEntry> {
    let mut entries = grouped_seeds(
        OPENCODE,
        OPENCODE_ANTHROPIC_MODELS,
        &ThinkingSupport::UnknownAdaptive,
    );
    entries.extend(grouped_seeds(
        OPENCODE,
        OPENCODE_RESPONSES_MODELS,
        &chat_ladder(),
    ));
    entries.extend(grouped_seeds(
        OPENCODE,
        OPENCODE_COMPLETIONS_MODELS,
        &chat_ladder(),
    ));
    entries.extend(grouped_seeds(
        OPENCODE,
        OPENCODE_GOOGLE_MODELS,
        &google_ladder(),
    ));
    entries
}

/// `OpenCode` Go rows. Anthropic-protocol ids ride
/// `https://opencode.ai/zen/go`, the rest `https://opencode.ai/zen/go/v1`.
///
/// `gemini-4-argon-contributor` is catalog-only by contract: it stays
/// listed (never [`Listing::Hidden`], which means routable-but-hidden)
/// while the row's wire table carries no Google selector, so selection
/// refuses it typed with `UnboundModel`.
fn opencode_go_seeds() -> Vec<CatalogEntry> {
    let mut entries = grouped_seeds(
        OPENCODE_GO,
        OPENCODE_GO_ANTHROPIC_MODELS,
        &ThinkingSupport::UnknownAdaptive,
    );
    entries.extend(grouped_seeds(
        OPENCODE_GO,
        OPENCODE_GO_RESPONSES_MODELS,
        &chat_ladder(),
    ));
    entries.extend(grouped_seeds(
        OPENCODE_GO,
        OPENCODE_GO_COMPLETIONS_MODELS,
        &chat_ladder(),
    ));
    entries.extend(grouped_seeds(
        OPENCODE_GO,
        OPENCODE_GO_GOOGLE_MODELS,
        &google_ladder(),
    ));
    entries
}

/// Vercel AI Gateway chat ids at `https://ai-gateway.vercel.sh`, served
/// over the Anthropic messages wire. Ids are the operator's vendor-prefixed
/// catalog names; the operator's model directory states no per-model token
/// limits dal could author, so every row carries `None` numbers. The seven
/// classifier ids stay census records and are not seeded.
fn vercel_ai_gateway_seeds() -> Vec<CatalogEntry> {
    grouped_seeds(
        VERCEL_AI_GATEWAY,
        VERCEL_AI_GATEWAY_MODELS,
        &ThinkingSupport::UnknownAdaptive,
    )
}

/// `OpenCode` Zen Anthropic-protocol ids (`https://opencode.ai/zen`, Zen docs).
const OPENCODE_ANTHROPIC_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-fable-5-1",
    "claude-haiku-4-5",
    "claude-haiku-5-5",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-opus-5-5",
    "claude-sonnet-4",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "claude-sonnet-5-5",
    "qwen3.5-plus",
    "qwen3.6-plus",
    "qwen3.8-flash",
];

/// `OpenCode` Zen Chat Completions ids (`https://opencode.ai/zen/v1`).
const OPENCODE_COMPLETIONS_MODELS: &[&str] = &[
    "big-pickle",
    "cat-jam-free",
    "deepseek-v4.1-flash",
    "deepseek-v4-flash",
    "deepseek-v4-flash-vision-exp",
    "deepseek-v4-pro",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "glm-5.3",
    "glm-5.3-flash",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "kimi-k3",
    "longcat-2.5-preview-free",
    "mimo-v2.6-flash-free",
    "minimax-m2.5",
    "minimax-m2.7",
    "minimax-m3",
    "mistral-large-4",
    "nemotron-3.5-lightning-free",
    "nemotron-3-ultra-free",
    "qwen3.8-max",
    "space-bunny-free",
    "step-5-preview-free",
];

/// `OpenCode` Zen Responses ids (`https://opencode.ai/zen/v1`).
const OPENCODE_RESPONSES_MODELS: &[&str] = &[
    "gpt-5",
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.3-codex",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.4-pro",
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5-codex",
    "gpt-5-nano",
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-6-sol",
    "grok-4.5",
    "grok-4.6",
    "grok-4.7",
    "grok-build-0.1",
    "muse-spark-1.2",
    "muse-spark-1.3",
    "muse-spark-1.3-contributor-free",
];

/// `OpenCode` Zen Google-protocol ids (`https://opencode.ai/zen/v1`).
const OPENCODE_GOOGLE_MODELS: &[&str] = &[
    "gemini-3.1-pro",
    "gemini-3.5-flash",
    "gemini-3.5-flash-lite",
    "gemini-3.6-flash",
    "gemini-3.7-flash",
    "gemini-3.8-flash",
    "gemini-3-flash",
    "gemini-4-argon",
];

/// `OpenCode` Go Anthropic-protocol ids (`https://opencode.ai/zen/go`).
const OPENCODE_GO_ANTHROPIC_MODELS: &[&str] = &[
    "claude-haiku-5-5",
    "minimax-m3",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];

/// `OpenCode` Go Chat Completions ids (`https://opencode.ai/zen/go/v1`).
const OPENCODE_GO_COMPLETIONS_MODELS: &[&str] = &[
    "cat-jam-free",
    "deepseek-v4.1-flash",
    "deepseek-v4-pro",
    "glm-5.2",
    "glm-5.3",
    "glm-5.3-flash",
    "hy3",
    "hy4-preview",
    "kimi-k2.7-code",
    "kimi-k3",
    "longcat-2.0",
    "longcat-2.5-preview-free",
    "mimo-v2.5",
    "mimo-v2.5-pro",
    "mimo-v2.6-flash",
    "mimo-v2.6-pro",
    "minimax-m2.7",
    "space-bunny",
    "step-5-preview-free",
];

/// `OpenCode` Go Responses ids (`https://opencode.ai/zen/go/v1`).
const OPENCODE_GO_RESPONSES_MODELS: &[&str] = &[
    "gpt-5.6-luna",
    "gpt-6-luna",
    "grok-4.6",
    "grok-4.7",
    "muse-spark-1.2-contributor",
    "muse-spark-1.3-contributor",
];

/// Vercel AI Gateway chat ids (`https://ai-gateway.vercel.sh`).
const VERCEL_AI_GATEWAY_MODELS: &[&str] = &[
    "alibaba/qwen-3-14b",
    "alibaba/qwen-3-235b",
    "alibaba/qwen-3-30b",
    "alibaba/qwen-3-32b",
    "alibaba/qwen3-coder",
    "alibaba/qwen3-coder-30b-a3b",
    "alibaba/qwen3-coder-next",
    "alibaba/qwen3-max-thinking",
    "alibaba/qwen3-next-80b-a3b-instruct",
    "alibaba/qwen3-next-80b-a3b-thinking",
    "alibaba/qwen3-vl-instruct",
    "alibaba/qwen3.5-flash",
    "alibaba/qwen3.5-plus",
    "alibaba/qwen3.6-27b",
    "alibaba/qwen3.6-plus",
    "alibaba/qwen3.7-flash",
    "alibaba/qwen3.7-max",
    "alibaba/qwen3.7-plus",
    "alibaba/qwen3.8-2.4t-a95b",
    "alibaba/qwen3.8-27b",
    "alibaba/qwen3.8-flash",
    "alibaba/qwen3.8-max",
    "alibaba/qwen3.8-max-0902",
    "alibaba/qwen3.8-max-prime",
    "alibaba/qwen3.8-omni-flash",
    "amazon/nova-2-lite",
    "amazon/nova-lite",
    "amazon/nova-micro",
    "amazon/nova-pro",
    "anthropic/claude-fable-5",
    "anthropic/claude-fable-5-batch",
    "anthropic/claude-fable-5.1",
    "anthropic/claude-fable-5.1-batch",
    "anthropic/claude-haiku-4.5",
    "anthropic/claude-haiku-4.5-batch",
    "anthropic/claude-haiku-5.5",
    "anthropic/claude-haiku-5.5-batch",
    "anthropic/claude-opus-4.5",
    "anthropic/claude-opus-4.5-batch",
    "anthropic/claude-opus-4.6",
    "anthropic/claude-opus-4.6-batch",
    "anthropic/claude-opus-4.7",
    "anthropic/claude-opus-4.7-batch",
    "anthropic/claude-opus-4.8",
    "anthropic/claude-opus-4.8-batch",
    "anthropic/claude-opus-4.8-fast",
    "anthropic/claude-opus-5",
    "anthropic/claude-opus-5-batch",
    "anthropic/claude-opus-5-fast",
    "anthropic/claude-opus-5.5",
    "anthropic/claude-opus-5.5-batch",
    "anthropic/claude-opus-5.5-fast",
    "anthropic/claude-sonnet-4",
    "anthropic/claude-sonnet-4.5",
    "anthropic/claude-sonnet-4.5-batch",
    "anthropic/claude-sonnet-4.6",
    "anthropic/claude-sonnet-4.6-batch",
    "anthropic/claude-sonnet-5",
    "anthropic/claude-sonnet-5-batch",
    "anthropic/claude-sonnet-5.5",
    "anthropic/claude-sonnet-5.5-batch",
    "arcee-ai/trinity-large-thinking",
    "bytedance/seed-1.6",
    "bytedance/seed-1.8",
    "bytedance/seed-2.1-turbo",
    "callstack/apex",
    "cohere/command-a",
    "deepseek/deepseek-r1",
    "deepseek/deepseek-v3.1",
    "deepseek/deepseek-v3.2",
    "deepseek/deepseek-v3.2-thinking",
    "deepseek/deepseek-v4-flash",
    "deepseek/deepseek-v4-flash-0731",
    "deepseek/deepseek-v4-flash-vision-exp",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-pro-0813",
    "deepseek/deepseek-v4.1-flash",
    "deepseek/deepseek-v4.1-flash-fast",
    "fireworks/ember-1",
    "google/gemini-2.5-flash",
    "google/gemini-2.5-flash-batch",
    "google/gemini-2.5-flash-lite",
    "google/gemini-2.5-flash-lite-batch",
    "google/gemini-2.5-pro",
    "google/gemini-2.5-pro-batch",
    "google/gemini-3-flash",
    "google/gemini-3-flash-batch",
    "google/gemini-3.1-flash-lite",
    "google/gemini-3.1-flash-lite-batch",
    "google/gemini-3.1-pro-preview",
    "google/gemini-3.1-pro-preview-batch",
    "google/gemini-3.5-flash",
    "google/gemini-3.5-flash-batch",
    "google/gemini-3.5-flash-lite",
    "google/gemini-3.5-flash-lite-batch",
    "google/gemini-3.6-flash",
    "google/gemini-3.6-flash-batch",
    "google/gemini-3.7-flash",
    "google/gemini-3.7-flash-batch",
    "google/gemini-3.8-flash",
    "google/gemini-4-argon",
    "google/gemma-4-26b-a4b-it",
    "google/gemma-4-31b-it",
    "inception/mercury-2",
    "inception/mercury-2.5",
    "inception/mercury-coder-small",
    "inclusionai/ling-3.0-flash",
    "inclusionai/ling-3.0-flash-fin",
    "inclusionai/ling-3.0-flash-sante",
    "inclusionai/ling-3.0-flash-vl",
    "inclusionai/ling-3.1-flash",
    "interfaze/interfaze-beta",
    "liquid/lfm2.5-24b-a2b",
    "meituan/longcat-2.5-preview",
    "meta/llama-3.1-70b",
    "meta/llama-3.1-8b",
    "meta/llama-3.3-70b",
    "meta/llama-4-maverick",
    "meta/llama-4-scout",
    "meta/muse-glimmer-30b",
    "meta/muse-spark-1.1",
    "meta/muse-spark-1.2",
    "meta/muse-spark-1.2-contributor",
    "meta/muse-spark-1.3",
    "meta/muse-spark-1.3-contributor",
    "meta/muse-wonder-1.0",
    "minimax/minimax-m2",
    "minimax/minimax-m2.1",
    "minimax/minimax-m2.1-lightning",
    "minimax/minimax-m2.5",
    "minimax/minimax-m2.5-highspeed",
    "minimax/minimax-m2.7",
    "minimax/minimax-m2.7-highspeed",
    "minimax/minimax-m3",
    "mistral/codestral",
    "mistral/ministral-14b",
    "mistral/ministral-3b",
    "mistral/ministral-8b",
    "mistral/mistral-large-3",
    "mistral/mistral-large-4",
    "mistral/mistral-medium-3.5",
    "mistral/mistral-nemo",
    "mistral/mistral-small",
    "mixedbread/toast-1",
    "moonshotai/kimi-k2",
    "moonshotai/kimi-k2.5",
    "moonshotai/kimi-k2.6",
    "moonshotai/kimi-k2.7-code",
    "moonshotai/kimi-k2.7-code-highspeed",
    "moonshotai/kimi-k3",
    "moonshotai/kimi-k3-fast",
    "nvidia/nemotron-3-nano-30b-a3b",
    "nvidia/nemotron-3-super-120b-a12b",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "nvidia/nemotron-3.5-lightning",
    "nvidia/nemotron-3.5-lightning-free",
    "nvidia/nemotron-nano-12b-v2-vl",
    "nvidia/nemotron-nano-9b-v2",
    "openai/gpt-3.5-turbo",
    "openai/gpt-4-turbo",
    "openai/gpt-4.1",
    "openai/gpt-4.1-batch",
    "openai/gpt-4.1-fast",
    "openai/gpt-4.1-mini",
    "openai/gpt-4.1-mini-batch",
    "openai/gpt-4.1-mini-fast",
    "openai/gpt-4.1-nano",
    "openai/gpt-4.1-nano-batch",
    "openai/gpt-4.1-nano-fast",
    "openai/gpt-4o",
    "openai/gpt-4o-batch",
    "openai/gpt-4o-fast",
    "openai/gpt-4o-mini",
    "openai/gpt-4o-mini-batch",
    "openai/gpt-4o-mini-fast",
    "openai/gpt-5",
    "openai/gpt-5-batch",
    "openai/gpt-5-codex",
    "openai/gpt-5-fast",
    "openai/gpt-5-mini",
    "openai/gpt-5-mini-batch",
    "openai/gpt-5-mini-fast",
    "openai/gpt-5-nano",
    "openai/gpt-5-nano-batch",
    "openai/gpt-5-pro",
    "openai/gpt-5-pro-batch",
    "openai/gpt-5.1-codex",
    "openai/gpt-5.1-codex-max",
    "openai/gpt-5.1-codex-mini",
    "openai/gpt-5.1-thinking",
    "openai/gpt-5.1-thinking-fast",
    "openai/gpt-5.2",
    "openai/gpt-5.2-batch",
    "openai/gpt-5.2-codex",
    "openai/gpt-5.2-fast",
    "openai/gpt-5.2-pro",
    "openai/gpt-5.3-codex",
    "openai/gpt-5.3-codex-fast",
    "openai/gpt-5.4",
    "openai/gpt-5.4-batch",
    "openai/gpt-5.4-fast",
    "openai/gpt-5.4-mini",
    "openai/gpt-5.4-mini-batch",
    "openai/gpt-5.4-mini-fast",
    "openai/gpt-5.4-nano",
    "openai/gpt-5.4-nano-batch",
    "openai/gpt-5.4-pro",
    "openai/gpt-5.5",
    "openai/gpt-5.5-batch",
    "openai/gpt-5.5-fast",
    "openai/gpt-5.5-pro",
    "openai/gpt-5.5-pro-batch",
    "openai/gpt-5.6-luna",
    "openai/gpt-5.6-luna-batch",
    "openai/gpt-5.6-luna-fast",
    "openai/gpt-5.6-sol",
    "openai/gpt-5.6-sol-batch",
    "openai/gpt-5.6-sol-fast",
    "openai/gpt-5.6-terra",
    "openai/gpt-5.6-terra-batch",
    "openai/gpt-5.6-terra-fast",
    "openai/gpt-6-astra",
    "openai/gpt-6-astra-batch",
    "openai/gpt-6-astra-fast",
    "openai/gpt-6-luna",
    "openai/gpt-6-luna-batch",
    "openai/gpt-6-luna-fast",
    "openai/gpt-6-sol",
    "openai/gpt-6-sol-batch",
    "openai/gpt-6-sol-fast",
    "openai/gpt-6.1-sol",
    "openai/gpt-6.1-sol-batch",
    "openai/gpt-6.1-sol-fast",
    "openai/gpt-oss-120b",
    "openai/gpt-oss-20b",
    "openai/gpt-oss-safeguard-120b",
    "openai/gpt-oss-safeguard-20b",
    "openai/o1",
    "openai/o3",
    "openai/o3-fast",
    "openai/o3-mini",
    "openai/o3-pro",
    "openai/o4-mini",
    "openai/o4-mini-fast",
    "poolside/laguna-s-2.1",
    "poolside/laguna-s-2.1-free",
    "quiverai/arrow-2",
    "quiverai/arrow-2-telos",
    "sakana/fugu-max",
    "sakana/fugu-ultra",
    "sakana/fugu-ultra-v2",
    "sakana/namazu",
    "spacexai/grok-4.1-fast-non-reasoning",
    "spacexai/grok-4.1-fast-reasoning",
    "spacexai/grok-4.20-multi-agent",
    "spacexai/grok-4.20-multi-agent-batch",
    "spacexai/grok-4.20-multi-agent-beta",
    "spacexai/grok-4.20-non-reasoning",
    "spacexai/grok-4.20-non-reasoning-batch",
    "spacexai/grok-4.20-non-reasoning-beta",
    "spacexai/grok-4.20-reasoning",
    "spacexai/grok-4.20-reasoning-batch",
    "spacexai/grok-4.20-reasoning-beta",
    "spacexai/grok-4.3",
    "spacexai/grok-4.3-batch",
    "spacexai/grok-4.5",
    "spacexai/grok-4.6",
    "spacexai/grok-4.7",
    "spacexai/grok-build-0.1",
    "stealth/glyph-cluster",
    "stepfun/step-3.7-flash",
    "tencent/hy3",
    "tencent/hy4-preview",
    "thinkingmachines/inkling",
    "thinkingmachines/inkling-small",
    "xiaomi/mimo-v2.5",
    "xiaomi/mimo-v2.5-pro",
    "xiaomi/mimo-v2.6-flash",
    "xiaomi/mimo-v2.6-pro",
    "xiaomi/mimo-v2.6-pro-ultraspeed",
    "zai/glm-4.5",
    "zai/glm-4.5-air",
    "zai/glm-4.5v",
    "zai/glm-4.6",
    "zai/glm-4.7",
    "zai/glm-4.7-flash",
    "zai/glm-4.7-flashx",
    "zai/glm-5",
    "zai/glm-5-turbo",
    "zai/glm-5.1",
    "zai/glm-5.2",
    "zai/glm-5.2-fast",
    "zai/glm-5.3",
    "zai/glm-5.3-fast",
    "zai/glm-5.3-flash",
    "zai/glm-5.3-flashx",
    "zai/glm-5v-turbo",
];

/// `OpenCode` Go's one Google-protocol id. Catalog-only: listed so it stays
/// visible, with no wire selector on its row.
const OPENCODE_GO_GOOGLE_MODELS: &[&str] = &["gemini-4-argon-contributor"];
