//! The API-key presets of the built-in provider table.
//!
//! A preset is a provider that takes one pasted or environment-supplied API
//! key, speaks one of the wire families dal implements, and serves every model
//! from one fixed endpoint. Its row carries what that needs and nothing else:
//! identity, family, base URL, the header style of the key, and the
//! environment variables that may hold the key, in resolve order. A provider
//! whose endpoint, wire, or required headers depend on the model, the account,
//! or the credential is not a preset.

use dal_core::Family;

use super::{FamilyPolicy, KeySpec, ProviderDef};
use crate::provider::{AuthStyle, Shape, Transport};

/// A preset row: HTTPS, one API key, no OAuth, the family's own header set.
const fn key_row(
    id: &'static str,
    name: &'static str,
    family: Family,
    base_url: &'static str,
    style: AuthStyle,
    env: &'static [&'static str],
) -> ProviderDef {
    ProviderDef {
        id,
        name,
        family,
        family_policy: FamilyPolicy::Compatible,
        base_url,
        transport: Transport::Https,
        key: Some(KeySpec { style, env }),
        oauth: None,
        shape: Shape::of_family(family),
    }
}

/// A Chat Completions provider; the key travels as a bearer token.
const fn chat(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    env: &'static [&'static str],
) -> ProviderDef {
    key_row(id, name, Family::Chat, base_url, AuthStyle::Bearer, env)
}

/// A Responses provider; the key travels as a bearer token.
const fn responses(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    env: &'static [&'static str],
) -> ProviderDef {
    key_row(
        id,
        name,
        Family::Responses,
        base_url,
        AuthStyle::Bearer,
        env,
    )
}

/// A Messages provider that expects the key as a bearer token.
const fn anthropic_bearer(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    env: &'static [&'static str],
) -> ProviderDef {
    key_row(
        id,
        name,
        Family::Anthropic,
        base_url,
        AuthStyle::Bearer,
        env,
    )
}

/// A Messages provider that expects the key in `x-api-key`.
const fn anthropic_key(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    env: &'static [&'static str],
) -> ProviderDef {
    key_row(
        id,
        name,
        Family::Anthropic,
        base_url,
        AuthStyle::XApiKey,
        env,
    )
}

/// The presets, by id. They follow the vendor rows in the table, so the first
/// row of each family stays the vendor's own.
pub(super) const PRESETS: &[ProviderDef] = &[
    responses(
        "abliteration",
        "Abliteration",
        "https://api.abliteration.ai/v1",
        &["ABLITERATION_API_KEY", "ABLIT_KEY"],
    ),
    chat(
        "aiand",
        "ai&",
        "https://api.aiand.com/v1",
        &["AIAND_API_KEY"],
    ),
    chat(
        "aimlapi",
        "AIML API",
        "https://api.aimlapi.com/v1",
        &["AIMLAPI_API_KEY"],
    ),
    chat(
        "ant-ling",
        "Ant Ling",
        "https://api.ant-ling.com/v1",
        &["ANT_LING_API_KEY"],
    ),
    chat(
        "baseten",
        "Baseten",
        "https://inference.baseten.co/v1",
        &["BASETEN_API_KEY"],
    ),
    chat(
        "charm-hyper",
        "Charm Hyper",
        "https://hyper.charm.land/v1",
        &["CHARM_HYPER_API_KEY", "HYPER_API_KEY"],
    ),
    chat(
        "coralbricks",
        "CoralBricks",
        "https://inference.coralbricks.ai/v1",
        &["CORAL_API_KEY", "CORALBRICKS_API_KEY"],
    ),
    chat(
        "deepinfra",
        "DeepInfra",
        "https://api.deepinfra.com/v1/openai",
        &["DEEPINFRA_API_KEY"],
    ),
    chat(
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com",
        &["DEEPSEEK_API_KEY"],
    ),
    chat(
        "firepass",
        "Fire Pass (Fireworks subscription)",
        "https://api.fireworks.ai/inference/v1",
        &["FIREPASS_API_KEY"],
    ),
    chat(
        "fireworks",
        "Fireworks",
        "https://api.fireworks.ai/inference/v1",
        &["FIREWORKS_API_KEY"],
    ),
    chat(
        "gmi-cloud",
        "GMI Cloud",
        "https://api.gmi-serving.com/v1",
        &["GMI_API_KEY"],
    ),
    chat(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        &["GROQ_API_KEY"],
    ),
    chat(
        "helmcode",
        "Helmcode",
        "https://api.helmcode.com/v1",
        &["HELMCODE_API_KEY"],
    ),
    chat(
        "huggingface",
        "Hugging Face Inference",
        "https://router.huggingface.co/v1",
        &["HUGGINGFACE_HUB_TOKEN", "HF_TOKEN"],
    ),
    chat(
        "llamacpp",
        "llama.cpp",
        "http://127.0.0.1:8080/v1",
        &["LLAMA_CPP_API_KEY"],
    ),
    responses(
        "meta",
        "Meta Model API",
        "https://api.meta.ai/v1",
        &["MODEL_API_KEY", "META_API_KEY"],
    ),
    anthropic_bearer(
        "minimax",
        "MiniMax",
        "https://api.minimax.io/anthropic",
        &["MINIMAX_API_KEY"],
    ),
    anthropic_bearer(
        "minimax-cn",
        "MiniMax CN",
        "https://api.minimaxi.com/anthropic",
        &["MINIMAX_CN_API_KEY"],
    ),
    anthropic_bearer(
        "minimax-code",
        "MiniMax Token Plan (International)",
        "https://api.minimax.io/anthropic",
        &["MINIMAX_CODE_API_KEY"],
    ),
    anthropic_bearer(
        "minimax-code-cn",
        "MiniMax Token Plan (China)",
        "https://api.minimaxi.com/anthropic",
        &["MINIMAX_CODE_CN_API_KEY"],
    ),
    chat(
        "moonshot",
        "Moonshot (Kimi API)",
        "https://api.moonshot.ai/v1",
        &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
    ),
    chat(
        "nanogpt",
        "NanoGPT",
        "https://nano-gpt.com/api/v1",
        &["NANO_GPT_API_KEY"],
    ),
    chat(
        "novita",
        "Novita",
        "https://api.novita.ai/openai/v1",
        &["NOVITA_API_KEY"],
    ),
    chat(
        "nvidia",
        "NVIDIA",
        "https://integrate.api.nvidia.com/v1",
        &["NVIDIA_API_KEY"],
    ),
    chat(
        "opengateway",
        "OpenGateway",
        "https://apis.opengateway.ai/v1",
        &["OPENGATEWAY_API_KEY"],
    ),
    chat(
        "qianfan",
        "Qianfan",
        "https://qianfan.baidubce.com/v2",
        &["QIANFAN_API_KEY"],
    ),
    chat(
        "qwen-portal",
        "Qwen Portal",
        "https://portal.qwen.ai/v1",
        &["QWEN_OAUTH_TOKEN", "QWEN_PORTAL_API_KEY"],
    ),
    chat(
        "qwen-token-plan",
        "Qwen Token Plan",
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        &["QWEN_TOKEN_PLAN_API_KEY"],
    ),
    chat(
        "qwen-token-plan-cn",
        "Qwen Token Plan CN",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        &["QWEN_TOKEN_PLAN_CN_API_KEY"],
    ),
    chat(
        "qwen-token-plan-individual",
        "Qwen Token Plan Individual",
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        &["QWEN_TOKEN_PLAN_API_KEY"],
    ),
    responses(
        "sakana",
        "Sakana AI",
        "https://api.sakana.ai/v1",
        &["SAKANA_API_KEY", "FUGU_API_KEY"],
    ),
    chat(
        "sambanova",
        "SambaNova",
        "https://api.sambanova.ai/v1",
        &["SAMBANOVA_API_KEY"],
    ),
    chat(
        "siliconflow",
        "SiliconFlow",
        "https://api.siliconflow.com/v1",
        &["SILICONFLOW_API_KEY"],
    ),
    chat(
        "siliconflow-cn",
        "SiliconFlow (China)",
        "https://api.siliconflow.cn/v1",
        &["SILICONFLOW_CN_API_KEY"],
    ),
    chat(
        "singularityapi-dev",
        "SingularityAPI",
        "https://api.singularityapi.dev/v1",
        &["SINGULARITYAPI_DEV_API_KEY"],
    ),
    chat(
        "singularityapi-tech",
        "SingularityAPI Reserved Lanes",
        "https://api.singularityapi.tech/v1",
        &["SINGULARITYAPI_TECH_API_KEY"],
    ),
    chat(
        "stepfun",
        "StepFun",
        "https://api.stepfun.ai/v1",
        &["STEPFUN_API_KEY"],
    ),
    chat(
        "synthetic",
        "Synthetic",
        "https://api.synthetic.new/openai/v1",
        &["SYNTHETIC_API_KEY"],
    ),
    chat(
        "together",
        "Together",
        "https://api.together.xyz/v1",
        &["TOGETHER_API_KEY"],
    ),
    anthropic_key(
        "umans",
        "Umans AI Coding Plan",
        "https://api.code.umans.ai",
        &["UMANS_AI_CODING_PLAN_API_KEY"],
    ),
    chat(
        "venice",
        "Venice",
        "https://api.venice.ai/api/v1",
        &["VENICE_API_KEY"],
    ),
    chat(
        "wafer-serverless",
        "Wafer Serverless (pay-as-you-go)",
        "https://pass.wafer.ai/v1",
        &["WAFER_SERVERLESS_API_KEY"],
    ),
    responses("xai", "xAI API", "https://api.x.ai/v1", &["XAI_API_KEY"]),
    chat(
        "xiaomi-token-plan-ams",
        "Xiaomi Token Plan (Europe)",
        "https://token-plan-ams.xiaomimimo.com/v1",
        &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
    ),
    chat(
        "xiaomi-token-plan-cn",
        "Xiaomi Token Plan (China)",
        "https://token-plan-cn.xiaomimimo.com/v1",
        &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
    ),
    chat(
        "xiaomi-token-plan-sgp",
        "Xiaomi Token Plan (Singapore)",
        "https://token-plan-sgp.xiaomimimo.com/v1",
        &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
    ),
    chat(
        "yolo-auto",
        "Yolo-Auto",
        "https://yolo-auto.com/v1",
        &["YOLO_AUTO_API_KEY"],
    ),
    chat(
        "zhipu-coding-plan",
        "Zhipu Coding Plan (智谱)",
        "https://open.bigmodel.cn/api/coding/paas/v4",
        &["ZHIPU_API_KEY"],
    ),
];
