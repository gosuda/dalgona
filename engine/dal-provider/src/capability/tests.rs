use super::*;

/// One dialect: the wrapped value, the kind it must report from the `Kind`
/// literal written next to it and from the `Api::KIND` constant, and its
/// persisted spelling.
struct Row {
    any: AnyApi,
    kind: Kind,
    api_kind: Kind,
    token: &'static str,
}

/// Lists the dialects of one kind. The `match` has no wildcard arm, so adding
/// a variant to the dialect enum fails to compile here until the new dialect
/// is spelled. The rows come from the same list as the `match`, so a variant
/// cannot be matched yet left out of the rows.
macro_rules! group {
    ($rows:ident, $wrap:ident, $api:ident, $kind:expr, [$($variant:ident => $token:literal),+ $(,)?]) => {{
        fn spelling(api: $api) -> &'static str {
            match api {
                $($api::$variant => $token),+
            }
        }
        $(
            $rows.push(Row {
                any: AnyApi::$wrap($api::$variant),
                kind: $kind,
                api_kind: <$api as Api>::KIND,
                token: spelling($api::$variant),
            });
        )+
    }};
}

/// The persisted spelling of every dialect of every kind, one row each.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    group!(rows, Image, ImageApi, Kind::Image, [
        OpenAiImages => "openai-images",
        OpenRouterImages => "openrouter-images",
        GoogleGenerativeAi => "google-generative-ai",
        GoogleGeminiCli => "google-gemini-cli",
        OpenAiResponses => "openai-responses",
        OpenAiCodexResponses => "openai-codex-responses",
    ]);
    group!(rows, Judge, JudgmentApi, Kind::Judge, [
        Typesafe => "typesafe",
        OpenRouterDecisions => "openrouter-decisions",
        WorkersAiSystemOne => "workers-ai-systemone",
        OpenAiDecisions => "openai-decisions",
        LlamaCppClassify => "llama-cpp-classify",
    ]);
    group!(rows, Embedding, EmbeddingApi, Kind::Embedding, [
        OpenAiEmbeddings => "openai-embeddings",
    ]);
    group!(rows, Speech, SpeechApi, Kind::Speech, [
        XaiTts => "xai-tts",
        OpenAiSpeech => "openai-speech",
        LocalInference => "local-inference",
    ]);
    group!(rows, Transcription, TranscriptionApi, Kind::Transcription, [
        OpenAiTranscriptions => "openai-transcriptions",
        LocalInference => "local-inference",
    ]);
    group!(rows, Rerank, RerankApi, Kind::Rerank, [
        OpenRouterRerank => "openrouter-rerank",
    ]);
    group!(rows, Video, VideoApi, Kind::Video, [
        OpenRouterVideo => "openrouter-video",
    ]);
    group!(rows, Grounding, GroundingApi, Kind::Grounding, [
        Gemini => "gemini",
        Anthropic => "anthropic",
        Codex => "codex",
        Xai => "xai",
        OpenRouter => "openrouter",
        OpenAi => "openai",
    ]);
    rows
}

/// The persisted spelling of every kind. Same exhaustiveness rule as
/// [`group!`].
fn kinds() -> Vec<(Kind, &'static str)> {
    macro_rules! kind_rows {
        ($($variant:ident => $token:literal),+ $(,)?) => {{
            fn spelling(kind: Kind) -> &'static str {
                match kind {
                    $(Kind::$variant => $token),+
                }
            }
            vec![$((Kind::$variant, spelling(Kind::$variant))),+]
        }};
    }
    kind_rows!(
        Image => "image",
        Judge => "judge",
        Embedding => "embedding",
        Speech => "speech",
        Transcription => "transcription",
        Rerank => "rerank",
        Video => "video",
        Grounding => "grounding",
    )
}

/// Resolves `token` inside the dialect enum of `kind`, the only way a caller
/// can name a dialect.
fn lookup(kind: Kind, token: &str) -> Option<AnyApi> {
    match kind {
        Kind::Image => ImageApi::from_token(token).map(AnyApi::Image),
        Kind::Judge => JudgmentApi::from_token(token).map(AnyApi::Judge),
        Kind::Embedding => EmbeddingApi::from_token(token).map(AnyApi::Embedding),
        Kind::Speech => SpeechApi::from_token(token).map(AnyApi::Speech),
        Kind::Transcription => TranscriptionApi::from_token(token).map(AnyApi::Transcription),
        Kind::Rerank => RerankApi::from_token(token).map(AnyApi::Rerank),
        Kind::Video => VideoApi::from_token(token).map(AnyApi::Video),
        Kind::Grounding => GroundingApi::from_token(token).map(AnyApi::Grounding),
    }
}

fn assert_no_problems(problems: &[String]) {
    assert!(
        problems.is_empty(),
        "{} problems: {problems:#?}",
        problems.len()
    );
}

#[test]
fn every_dialect_reports_its_kind_spells_itself_and_resolves_back_to_itself() {
    let mut problems = Vec::new();
    for row in rows() {
        let Row {
            any,
            kind,
            api_kind,
            token,
        } = row;
        if any.kind() != kind {
            problems.push(format!(
                "{any:?}: AnyApi::kind is {:?}, not {kind:?}",
                any.kind()
            ));
        }
        if api_kind != kind {
            problems.push(format!("{any:?}: Api::KIND is {api_kind:?}, not {kind:?}"));
        }
        if any.token() != token {
            problems.push(format!(
                "{any:?}: token is {:?}, not {token:?}",
                any.token()
            ));
        }
        if lookup(kind, token) != Some(any) {
            problems.push(format!(
                "{any:?}: lookup of {token:?} in {kind:?} gave {:?}",
                lookup(kind, token)
            ));
        }
        let shaped = !token.is_empty()
            && token
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !token.starts_with('-')
            && !token.ends_with('-')
            && !token.contains("--");
        if !shaped {
            problems.push(format!("{any:?}: {token:?} is not a lowercase kebab token"));
        }
    }
    let rows = rows();
    for (index, left) in rows.iter().enumerate() {
        for right in &rows[index + 1..] {
            if left.kind == right.kind && left.token == right.token {
                problems.push(format!(
                    "{:?} and {:?} share the spelling {:?} inside {:?}",
                    left.any, right.any, left.token, left.kind
                ));
            }
        }
    }
    assert_no_problems(&problems);
}

#[test]
fn token_lookup_is_scoped_to_its_kind_across_the_whole_vocabulary() {
    let rows = rows();
    let mut problems = Vec::new();
    for (kind, _) in kinds() {
        for row in &rows {
            let owned = rows
                .iter()
                .any(|other| other.kind == kind && other.token == row.token);
            let found = lookup(kind, row.token);
            if found.is_some() != owned {
                problems.push(format!(
                    "{kind:?} lookup of {:?} (a {:?} spelling) gave {found:?}, expected owned = {owned}",
                    row.token, row.kind
                ));
            }
            if let Some(found) = found
                && (found.kind() != kind || found.token() != row.token)
            {
                problems.push(format!(
                    "{kind:?} lookup of {:?} returned {found:?} of another kind or spelling",
                    row.token
                ));
            }
        }
    }
    let mut shared: Vec<(&str, Vec<Kind>)> = Vec::new();
    for row in &rows {
        let owners: Vec<Kind> = rows
            .iter()
            .filter(|other| other.token == row.token)
            .map(|other| other.kind)
            .collect();
        if owners.len() > 1 && !shared.iter().any(|(token, _)| *token == row.token) {
            shared.push((row.token, owners));
        }
    }
    if shared != [("local-inference", vec![Kind::Speech, Kind::Transcription])] {
        problems.push(format!(
            "only local-inference may be spelled by two kinds, got {shared:?}"
        ));
    }
    assert_ne!(
        AnyApi::Speech(SpeechApi::LocalInference),
        AnyApi::Transcription(TranscriptionApi::LocalInference),
        "one spelling under two kinds stays two distinct dialects"
    );
    assert_no_problems(&problems);
}

#[test]
fn tokens_are_exact_spellings_never_normalised() {
    let mut problems = Vec::new();
    let mut probe = |kind: Kind, valid: &str, hostile: String, resolved: Option<AnyApi>| {
        let owned = rows()
            .iter()
            .any(|other| other.kind == kind && other.token == hostile);
        if resolved.is_some() && !owned {
            problems.push(format!(
                "{kind:?} accepted the near-miss {hostile:?} of {valid:?} as {resolved:?}"
            ));
        }
    };
    for row in rows() {
        let token = row.token;
        let mut hostile = vec![
            token.to_ascii_uppercase(),
            format!(" {token}"),
            format!("{token} "),
            format!("{token}\n"),
            format!("{token}\0"),
            format!("{token}x"),
            token[..token.len() - 1].to_owned(),
            token.replace('-', "_"),
            token.replace('-', ""),
        ];
        hostile.push(
            token
                .char_indices()
                .map(|(at, c)| if at == 0 { c.to_ascii_uppercase() } else { c })
                .collect(),
        );
        for near_miss in hostile {
            let resolved = lookup(row.kind, &near_miss);
            probe(row.kind, token, near_miss, resolved);
        }
    }
    for (_, token) in kinds() {
        for near_miss in [
            token.to_ascii_uppercase(),
            format!(" {token}"),
            format!("{token} "),
            format!("{token}\n"),
            format!("{token}s"),
            token[..token.len() - 1].to_owned(),
        ] {
            if let Some(found) = Kind::from_token(&near_miss) {
                problems.push(format!(
                    "Kind accepted the near-miss {near_miss:?} of {token:?} as {found:?}"
                ));
            }
        }
    }
    assert_no_problems(&problems);
}

#[test]
fn kind_spellings_round_trip_cover_every_dialect_and_collide_with_none() {
    let mut problems = Vec::new();
    for (kind, token) in kinds() {
        if kind.token() != token {
            problems.push(format!(
                "{kind:?}: token is {:?}, not {token:?}",
                kind.token()
            ));
        }
        if Kind::from_token(token) != Some(kind) {
            problems.push(format!(
                "{kind:?}: from_token({token:?}) gave {:?}",
                Kind::from_token(token)
            ));
        }
        if !rows().iter().any(|row| row.kind == kind) {
            problems.push(format!("{kind:?} has no dialect row"));
        }
    }
    for row in rows() {
        if !kinds().iter().any(|(kind, _)| *kind == row.kind) {
            problems.push(format!(
                "{:?} names a kind missing from the kind table",
                row.any
            ));
        }
        if let Some(kind) = Kind::from_token(row.token) {
            problems.push(format!(
                "dialect spelling {:?} also reads as the kind {kind:?}",
                row.token
            ));
        }
    }
    assert_eq!(Kind::from_token(""), None);
    assert_no_problems(&problems);
}

// The nine example-table tests below were first consolidated into the
// exhaustive matrix above. Mutation runs showed seven of them are
// load-bearing (each token or KIND mutation turns one red) and the other two
// assert cross-kind rejections no mutation proved vacuous, so by the deletion
// rule they stay. See /tmp/offensive-engine.md section on capability.

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
fn media_new_moves_its_buffer_without_copying() {
    let owned = vec![7_u8; 64];
    let at = owned.as_ptr();
    let media = Media::new("image/png", owned).unwrap();
    assert_eq!(media.bytes.as_ptr(), at, "a Vec moves in without a copy");

    let shared = bytes::Bytes::from(vec![9_u8; 64]);
    let at = shared.as_ptr();
    let media = Media::new("audio/wav", shared).unwrap();
    assert_eq!(media.bytes.as_ptr(), at, "a Bytes keeps its storage");
    assert_eq!(media.clone().bytes.as_ptr(), at, "a clone shares it");
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
        "image/png\u{a0}",
        "image/\u{2003}png",
        "image/png\u{2028}",
        "image\t/png",
        "image/png\u{85}",
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
