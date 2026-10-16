use super::*;

#[test]
fn kind_tokens_roundtrip() {
    let cases = [
        (Kind::Image, "image"),
        (Kind::Judge, "judge"),
        (Kind::Embedding, "embedding"),
        (Kind::Speech, "speech"),
        (Kind::Transcription, "transcription"),
        (Kind::Rerank, "rerank"),
        (Kind::Video, "video"),
        (Kind::Grounding, "grounding"),
    ];
    for (kind, token) in cases {
        assert_eq!(kind.token(), token);
        assert_eq!(Kind::from_token(token), Some(kind));
    }
    assert_eq!(Kind::from_token("local-inference"), None);
    assert_eq!(Kind::from_token(""), None);
}

#[test]
fn image_tokens_roundtrip() {
    let cases = [
        (ImageApi::OpenAiImages, "openai-images"),
        (ImageApi::OpenRouterImages, "openrouter-images"),
        (ImageApi::GoogleGenerativeAi, "google-generative-ai"),
        (ImageApi::GoogleGeminiCli, "google-gemini-cli"),
        (ImageApi::OpenAiResponses, "openai-responses"),
        (ImageApi::OpenAiCodexResponses, "openai-codex-responses"),
    ];
    for (api, token) in cases {
        assert_eq!(api.token(), token);
        assert_eq!(ImageApi::from_token(token), Some(api));
        assert_eq!(ImageApi::KIND, Kind::Image);
    }
    assert_eq!(ImageApi::from_token("local-inference"), None);
    assert_eq!(ImageApi::from_token(""), None);
}

#[test]
fn judgment_tokens_roundtrip() {
    let cases = [
        (JudgmentApi::Typesafe, "typesafe"),
        (JudgmentApi::OpenRouterDecisions, "openrouter-decisions"),
        (JudgmentApi::WorkersAiSystemOne, "workers-ai-systemone"),
        (JudgmentApi::OpenAiDecisions, "openai-decisions"),
        (JudgmentApi::LlamaCppClassify, "llama-cpp-classify"),
    ];
    for (api, token) in cases {
        assert_eq!(api.token(), token);
        assert_eq!(JudgmentApi::from_token(token), Some(api));
        assert_eq!(JudgmentApi::KIND, Kind::Judge);
    }
    assert_eq!(JudgmentApi::from_token("local-inference"), None);
}

#[test]
fn judgment_tokens_are_distinct() {
    assert_eq!(
        JudgmentApi::from_token("openrouter-decisions"),
        Some(JudgmentApi::OpenRouterDecisions)
    );
    assert_eq!(
        JudgmentApi::from_token("openai-decisions"),
        Some(JudgmentApi::OpenAiDecisions)
    );
    assert_eq!(
        JudgmentApi::from_token("openai-images"),
        None,
        "judgment lookup must not resolve an image token"
    );
}

#[test]
fn single_and_small_enum_tokens_roundtrip() {
    assert_eq!(EmbeddingApi::OpenAiEmbeddings.token(), "openai-embeddings");
    assert_eq!(
        EmbeddingApi::from_token("openai-embeddings"),
        Some(EmbeddingApi::OpenAiEmbeddings)
    );
    assert_eq!(EmbeddingApi::KIND, Kind::Embedding);
    assert_eq!(RerankApi::OpenRouterRerank.token(), "openrouter-rerank");
    assert_eq!(
        RerankApi::from_token("openrouter-rerank"),
        Some(RerankApi::OpenRouterRerank)
    );
    assert_eq!(RerankApi::KIND, Kind::Rerank);
    assert_eq!(VideoApi::OpenRouterVideo.token(), "openrouter-video");
    assert_eq!(
        VideoApi::from_token("openrouter-video"),
        Some(VideoApi::OpenRouterVideo)
    );
    assert_eq!(VideoApi::KIND, Kind::Video);
    assert_eq!(EmbeddingApi::from_token("local-inference"), None);
    assert_eq!(RerankApi::from_token("openai-embeddings"), None);
}

#[test]
fn grounding_tokens_roundtrip() {
    let cases = [
        (GroundingApi::Gemini, "gemini"),
        (GroundingApi::Anthropic, "anthropic"),
        (GroundingApi::Codex, "codex"),
        (GroundingApi::Xai, "xai"),
        (GroundingApi::OpenRouter, "openrouter"),
        (GroundingApi::OpenAi, "openai"),
    ];
    for (api, token) in cases {
        assert_eq!(api.token(), token);
        assert_eq!(GroundingApi::from_token(token), Some(api));
        assert_eq!(GroundingApi::KIND, Kind::Grounding);
    }
    assert_eq!(GroundingApi::from_token("openai-images"), None);
}

#[test]
fn local_inference_resolves_only_in_its_kinds() {
    let speech = SpeechApi::from_token("local-inference");
    let transcription = TranscriptionApi::from_token("local-inference");
    assert_eq!(speech, Some(SpeechApi::LocalInference));
    assert_eq!(transcription, Some(TranscriptionApi::LocalInference));
    assert_eq!(SpeechApi::KIND, Kind::Speech);
    assert_eq!(TranscriptionApi::KIND, Kind::Transcription);
    assert_eq!(SpeechApi::LocalInference.token(), "local-inference");
    assert_eq!(TranscriptionApi::LocalInference.token(), "local-inference");
    assert_eq!(ImageApi::from_token("local-inference"), None);
    assert_eq!(EmbeddingApi::from_token("local-inference"), None);
    assert_eq!(RerankApi::from_token("local-inference"), None);
    assert_eq!(VideoApi::from_token("local-inference"), None);
    assert_eq!(GroundingApi::from_token("local-inference"), None);
    assert_eq!(JudgmentApi::from_token("local-inference"), None);
}

#[test]
fn speech_and_transcription_other_tokens_roundtrip() {
    assert_eq!(SpeechApi::XaiTts.token(), "xai-tts");
    assert_eq!(SpeechApi::from_token("xai-tts"), Some(SpeechApi::XaiTts));
    assert_eq!(SpeechApi::OpenAiSpeech.token(), "openai-speech");
    assert_eq!(
        SpeechApi::from_token("openai-speech"),
        Some(SpeechApi::OpenAiSpeech)
    );
    assert_eq!(
        TranscriptionApi::OpenAiTranscriptions.token(),
        "openai-transcriptions"
    );
    assert_eq!(
        TranscriptionApi::from_token("openai-transcriptions"),
        Some(TranscriptionApi::OpenAiTranscriptions)
    );
    assert_eq!(
        SpeechApi::from_token("openai-transcriptions"),
        None,
        "speech lookup must not resolve a transcription token"
    );
    assert_eq!(
        TranscriptionApi::from_token("xai-tts"),
        None,
        "transcription lookup must not resolve a speech token"
    );
}

#[test]
fn any_api_kind_and_token_follow_inner() {
    let cases: &[(AnyApi, Kind, &str)] = &[
        (
            AnyApi::Image(ImageApi::OpenAiImages),
            Kind::Image,
            "openai-images",
        ),
        (
            AnyApi::Judge(JudgmentApi::Typesafe),
            Kind::Judge,
            "typesafe",
        ),
        (
            AnyApi::Embedding(EmbeddingApi::OpenAiEmbeddings),
            Kind::Embedding,
            "openai-embeddings",
        ),
        (
            AnyApi::Speech(SpeechApi::LocalInference),
            Kind::Speech,
            "local-inference",
        ),
        (
            AnyApi::Transcription(TranscriptionApi::LocalInference),
            Kind::Transcription,
            "local-inference",
        ),
        (
            AnyApi::Rerank(RerankApi::OpenRouterRerank),
            Kind::Rerank,
            "openrouter-rerank",
        ),
        (
            AnyApi::Video(VideoApi::OpenRouterVideo),
            Kind::Video,
            "openrouter-video",
        ),
        (
            AnyApi::Grounding(GroundingApi::Gemini),
            Kind::Grounding,
            "gemini",
        ),
    ];
    for (any, kind, token) in cases {
        assert_eq!(any.kind(), *kind);
        assert_eq!(any.token(), *token);
        assert_eq!(any.kind().token(), kind.token());
    }
    let speech = AnyApi::Speech(SpeechApi::LocalInference);
    let transcription = AnyApi::Transcription(TranscriptionApi::LocalInference);
    assert_eq!(speech.token(), transcription.token());
    assert_ne!(speech.kind(), transcription.kind());
}

#[test]
fn media_new_keeps_valid_mime_and_bytes() {
    let media = Media::new("image/png", vec![1_u8, 2, 3]).unwrap();
    assert_eq!(media.mime.as_ref(), "image/png");
    assert_eq!(media.bytes.as_ref(), &[1_u8, 2, 3]);
    let shared = bytes::Bytes::from_static(b"audio");
    let media = Media::new("audio/wav", shared).unwrap();
    assert_eq!(media.mime.as_ref(), "audio/wav");
    assert_eq!(media.bytes.as_ref(), b"audio");
}

#[test]
fn media_new_rejects_empty_mime() {
    let error = Media::new("", vec![0_u8]).unwrap_err();
    assert_eq!(error, MediaError::EmptyMime);
}

#[test]
fn media_new_rejects_malformed_mime() {
    let cases = [
        "png",
        "image/",
        "/png",
        "image/png/extra",
        "image//png",
        "image/png ",
        " image/png",
        "image/png\n",
        "image/\u{0}png",
        "text/plain;charset=utf-8 ",
        "image/png\u{7f}",
    ];
    for mime in cases {
        let error = Media::new(mime, vec![0_u8]).unwrap_err();
        assert_eq!(
            error,
            MediaError::InvalidMime { mime: mime.into() },
            "mime {mime:?} must be refused"
        );
    }
    assert_eq!(
        Media::new("text/plain", vec![0_u8]).unwrap().mime.as_ref(),
        "text/plain"
    );
    assert_eq!(
        Media::new("video/*", vec![0_u8]).unwrap().mime.as_ref(),
        "video/*"
    );
}
