//! Kind-safe wire dialect identity for auxiliary capabilities.
//!
//! Each capability kind keeps its own closed dialect enum, so the same
//! persisted token under two kinds never collides. The dialect token
//! `local-inference` names a speech wire under [`SpeechApi`] and a separate
//! transcription wire under [`TranscriptionApi`]; resolving it requires the
//! caller to name the kind first by picking the enum. No auxiliary kind is a
//! chat family and no global token registry exists here.

mod sealed {
    /// Seals [`super::Api`] so only this crate implements it.
    pub trait Sealed {}
}

impl sealed::Sealed for ImageApi {}
impl sealed::Sealed for JudgmentApi {}
impl sealed::Sealed for EmbeddingApi {}
impl sealed::Sealed for SpeechApi {}
impl sealed::Sealed for TranscriptionApi {}
impl sealed::Sealed for RerankApi {}
impl sealed::Sealed for VideoApi {}
impl sealed::Sealed for GroundingApi {}

/// What an auxiliary call produces.
///
/// Tokens are the persisted and configuration spellings.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Kind {
    /// Image generation and editing.
    Image,
    /// Native judgments and classifiers.
    Judge,
    /// Text and token embeddings.
    Embedding,
    /// Speech synthesis.
    Speech,
    /// Audio transcription.
    Transcription,
    /// Document reranking.
    Rerank,
    /// Video generation jobs.
    Video,
    /// Standalone grounded answers.
    Grounding,
}

impl Kind {
    /// The persisted and configuration spelling of this kind.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Judge => "judge",
            Self::Embedding => "embedding",
            Self::Speech => "speech",
            Self::Transcription => "transcription",
            Self::Rerank => "rerank",
            Self::Video => "video",
            Self::Grounding => "grounding",
        }
    }

    /// The kind for a persisted spelling, or `None` when unknown.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "image" => Some(Self::Image),
            "judge" => Some(Self::Judge),
            "embedding" => Some(Self::Embedding),
            "speech" => Some(Self::Speech),
            "transcription" => Some(Self::Transcription),
            "rerank" => Some(Self::Rerank),
            "video" => Some(Self::Video),
            "grounding" => Some(Self::Grounding),
            _ => None,
        }
    }
}

/// Wire dialect of one capability kind.
///
/// Sealed: only this crate implements it. Each kind has its own closed enum,
/// so a [`SpeechApi`] value can never flow into a transcription call even
/// though both spell a `local-inference` dialect.
pub trait Api: sealed::Sealed + Copy + Eq + std::fmt::Debug + Send + Sync + 'static {
    /// The capability kind this dialect belongs to.
    const KIND: Kind;

    /// The persisted and configuration spelling of this dialect.
    fn token(self) -> &'static str;

    /// The dialect for a persisted spelling, or `None` when unknown.
    ///
    /// Lookup is scoped to this kind: `local-inference` resolves under
    /// [`SpeechApi`] and under [`TranscriptionApi`] independently, and under
    /// no other enum.
    fn from_token(token: &str) -> Option<Self>;
}

/// Image wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImageApi {
    /// `OpenAI` image generation and editing.
    OpenAiImages,
    /// `OpenRouter` image generation.
    OpenRouterImages,
    /// Google generative image generation.
    GoogleGenerativeAi,
    /// Google Gemini CLI internal image generation.
    GoogleGeminiCli,
    /// `OpenAI` Responses hosted image tool.
    OpenAiResponses,
    /// `OpenAI` Codex Responses hosted image tool.
    OpenAiCodexResponses,
}

impl Api for ImageApi {
    const KIND: Kind = Kind::Image;

    fn token(self) -> &'static str {
        match self {
            Self::OpenAiImages => "openai-images",
            Self::OpenRouterImages => "openrouter-images",
            Self::GoogleGenerativeAi => "google-generative-ai",
            Self::GoogleGeminiCli => "google-gemini-cli",
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiCodexResponses => "openai-codex-responses",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "openai-images" => Some(Self::OpenAiImages),
            "openrouter-images" => Some(Self::OpenRouterImages),
            "google-generative-ai" => Some(Self::GoogleGenerativeAi),
            "google-gemini-cli" => Some(Self::GoogleGeminiCli),
            "openai-responses" => Some(Self::OpenAiResponses),
            "openai-codex-responses" => Some(Self::OpenAiCodexResponses),
            _ => None,
        }
    }
}

/// Native judgment and classifier wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum JudgmentApi {
    /// `TypeSafe` native judgment.
    Typesafe,
    /// `OpenRouter` decisions.
    OpenRouterDecisions,
    /// Cloudflare Workers AI system-one judgment.
    WorkersAiSystemOne,
    /// `OpenAI` decisions.
    OpenAiDecisions,
    /// Local llama.cpp classifier.
    LlamaCppClassify,
}

impl Api for JudgmentApi {
    const KIND: Kind = Kind::Judge;

    fn token(self) -> &'static str {
        match self {
            Self::Typesafe => "typesafe",
            Self::OpenRouterDecisions => "openrouter-decisions",
            Self::WorkersAiSystemOne => "workers-ai-systemone",
            Self::OpenAiDecisions => "openai-decisions",
            Self::LlamaCppClassify => "llama-cpp-classify",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "typesafe" => Some(Self::Typesafe),
            "openrouter-decisions" => Some(Self::OpenRouterDecisions),
            "workers-ai-systemone" => Some(Self::WorkersAiSystemOne),
            "openai-decisions" => Some(Self::OpenAiDecisions),
            "llama-cpp-classify" => Some(Self::LlamaCppClassify),
            _ => None,
        }
    }
}

/// Embedding wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EmbeddingApi {
    /// `OpenAI` embeddings.
    OpenAiEmbeddings,
}

impl Api for EmbeddingApi {
    const KIND: Kind = Kind::Embedding;

    fn token(self) -> &'static str {
        match self {
            Self::OpenAiEmbeddings => "openai-embeddings",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "openai-embeddings" => Some(Self::OpenAiEmbeddings),
            _ => None,
        }
    }
}

/// Speech synthesis wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SpeechApi {
    /// xAI text to speech.
    XaiTts,
    /// `OpenAI` speech synthesis.
    OpenAiSpeech,
    /// On-device synthesis; running it needs an engine chosen elsewhere.
    LocalInference,
}

impl Api for SpeechApi {
    const KIND: Kind = Kind::Speech;

    fn token(self) -> &'static str {
        match self {
            Self::XaiTts => "xai-tts",
            Self::OpenAiSpeech => "openai-speech",
            Self::LocalInference => "local-inference",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "xai-tts" => Some(Self::XaiTts),
            "openai-speech" => Some(Self::OpenAiSpeech),
            "local-inference" => Some(Self::LocalInference),
            _ => None,
        }
    }
}

/// Transcription wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TranscriptionApi {
    /// `OpenAI` transcription.
    OpenAiTranscriptions,
    /// On-device transcription; running it needs an engine chosen elsewhere.
    LocalInference,
}

impl Api for TranscriptionApi {
    const KIND: Kind = Kind::Transcription;

    fn token(self) -> &'static str {
        match self {
            Self::OpenAiTranscriptions => "openai-transcriptions",
            Self::LocalInference => "local-inference",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "openai-transcriptions" => Some(Self::OpenAiTranscriptions),
            "local-inference" => Some(Self::LocalInference),
            _ => None,
        }
    }
}

/// Rerank wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RerankApi {
    /// `OpenRouter` rerank.
    OpenRouterRerank,
}

impl Api for RerankApi {
    const KIND: Kind = Kind::Rerank;

    fn token(self) -> &'static str {
        match self {
            Self::OpenRouterRerank => "openrouter-rerank",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "openrouter-rerank" => Some(Self::OpenRouterRerank),
            _ => None,
        }
    }
}

/// Video generation wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VideoApi {
    /// `OpenRouter` video generation.
    OpenRouterVideo,
}

impl Api for VideoApi {
    const KIND: Kind = Kind::Video;

    fn token(self) -> &'static str {
        match self {
            Self::OpenRouterVideo => "openrouter-video",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "openrouter-video" => Some(Self::OpenRouterVideo),
            _ => None,
        }
    }
}

/// Standalone grounded-answer wire dialects.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GroundingApi {
    /// Gemini grounding.
    Gemini,
    /// Anthropic grounding.
    Anthropic,
    /// Codex grounding.
    Codex,
    /// xAI grounding.
    Xai,
    /// `OpenRouter` grounding.
    OpenRouter,
    /// `OpenAI` grounding.
    OpenAi,
}

impl Api for GroundingApi {
    const KIND: Kind = Kind::Grounding;

    fn token(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
            Self::Xai => "xai",
            Self::OpenRouter => "openrouter",
            Self::OpenAi => "openai",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "gemini" => Some(Self::Gemini),
            "anthropic" => Some(Self::Anthropic),
            "codex" => Some(Self::Codex),
            "xai" => Some(Self::Xai),
            "openrouter" => Some(Self::OpenRouter),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }
}

/// Every dialect of every kind, for catalog rows and listings.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnyApi {
    /// An image wire dialect.
    Image(ImageApi),
    /// A judgment wire dialect.
    Judge(JudgmentApi),
    /// An embedding wire dialect.
    Embedding(EmbeddingApi),
    /// A speech wire dialect.
    Speech(SpeechApi),
    /// A transcription wire dialect.
    Transcription(TranscriptionApi),
    /// A rerank wire dialect.
    Rerank(RerankApi),
    /// A video wire dialect.
    Video(VideoApi),
    /// A grounding wire dialect.
    Grounding(GroundingApi),
}

impl AnyApi {
    /// The kind the wrapped dialect belongs to.
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self {
            Self::Image(_) => Kind::Image,
            Self::Judge(_) => Kind::Judge,
            Self::Embedding(_) => Kind::Embedding,
            Self::Speech(_) => Kind::Speech,
            Self::Transcription(_) => Kind::Transcription,
            Self::Rerank(_) => Kind::Rerank,
            Self::Video(_) => Kind::Video,
            Self::Grounding(_) => Kind::Grounding,
        }
    }

    /// The persisted spelling of the wrapped dialect.
    ///
    /// Both [`SpeechApi::LocalInference`] and
    /// [`TranscriptionApi::LocalInference`] spell `local-inference`; the
    /// spelling alone does not name the kind.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Image(api) => api.token(),
            Self::Judge(api) => api.token(),
            Self::Embedding(api) => api.token(),
            Self::Speech(api) => api.token(),
            Self::Transcription(api) => api.token(),
            Self::Rerank(api) => api.token(),
            Self::Video(api) => api.token(),
            Self::Grounding(api) => api.token(),
        }
    }
}
