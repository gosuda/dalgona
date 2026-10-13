use super::scan::{Member, Members, scan_items, scan_members};
use super::{
    Block, CallId, DecodeError, Deserialize, EntryId, InferredPurpose, JournalPart, ModelRoute,
    NonZeroU64, Owner, RawJson, Usage,
};

// Decode side: one member scan, `"v"` before `"type"`. Record members a
// format-1 record does not declare are dropped; the closed nested
// payloads (`usage`, parts, blocks, `who`, `purpose`, `answer`, `from`,
// `outcome`, `stop`, `route`) are scanned member by member and stay
// strict.
//
// Values stay borrowed slices parsed once into their typed shape: no
// intermediate DOM is built. Offsets and error shapes match the previous
// DOM walk exactly.

pub(super) fn invalid(offset: usize, message: impl Into<Box<str>>) -> DecodeError {
    DecodeError::Invalid {
        offset,
        message: message.into(),
    }
}

pub(super) fn need<'a>(members: &mut Members<'a>, name: &str) -> Result<Member<'a>, DecodeError> {
    members
        .iter()
        .position(|member| member.name.as_ref() == name)
        .map(|pos| members.swap_remove(pos))
        .ok_or_else(|| invalid(0, format!("missing member `{name}`")))
}

pub(super) fn want<'a>(members: &mut Members<'a>, name: &str) -> Option<Member<'a>> {
    members
        .iter()
        .position(|member| member.name.as_ref() == name)
        .map(|pos| members.swap_remove(pos))
}

pub(super) fn object_members<'a>(
    raw: &'a str,
    base_offset: usize,
    seen: &[&str],
) -> Result<Members<'a>, DecodeError> {
    let mut members = Members::new();
    for member in scan_members(raw, base_offset)? {
        if !seen.contains(&member.name.as_ref()) {
            return Err(invalid(
                member.offset,
                format!("unknown member `{}`", member.name),
            ));
        }
        if members
            .iter()
            .any(|known: &Member<'_>| known.name == member.name)
        {
            return Err(invalid(
                member.offset,
                format!("duplicate member `{}`", member.name),
            ));
        }
        members.push(member);
    }
    Ok(members)
}
pub(super) fn json_member<T: for<'de> Deserialize<'de>>(
    member: &Member<'_>,
) -> Result<T, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    sonic_rs::from_slice(raw.as_bytes()).map_err(|error| invalid(offset, error.to_string()))
}

pub(super) fn text_member(member: &Member<'_>, name: &str) -> Result<Box<str>, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    sonic_rs::from_slice::<String>(raw.as_bytes())
        .map(String::into_boxed_str)
        .map_err(|_| invalid(offset, format!("member `{name}` must be a string")))
}

pub(super) fn bool_member(member: &Member<'_>, name: &str) -> Result<bool, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(
            offset,
            format!("member `{name}` must be a boolean"),
        )),
    }
}

pub(super) fn u64_member(member: &Member<'_>, name: &str) -> Result<u64, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    parse_u64(raw).ok_or_else(|| {
        invalid(
            offset,
            format!("member `{name}` must be an unsigned integer"),
        )
    })
}

pub(super) fn i32_member(member: &Member<'_>, name: &str) -> Result<i32, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    parse_i64(raw)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid(offset, format!("member `{name}` must be an i32")))
}

pub(super) fn is_null(member: &Member<'_>) -> bool {
    member.value == "null"
}

pub(super) fn entry_id_member(member: &Member<'_>, name: &str) -> Result<EntryId, DecodeError> {
    let value = parse_u64(member.value).ok_or_else(|| {
        invalid(
            member.offset,
            format!("member `{name}` must be an unsigned integer"),
        )
    })?;
    NonZeroU64::new(value).map(EntryId::new).ok_or_else(|| {
        invalid(
            member.offset,
            format!("member `{name}` must be a positive integer"),
        )
    })
}

pub(super) fn opt_entry_id_member(
    member: &Member<'_>,
    name: &str,
) -> Result<Option<EntryId>, DecodeError> {
    if is_null(member) {
        Ok(None)
    } else {
        entry_id_member(member, name).map(Some)
    }
}

pub(super) fn opt_text_member(
    member: &Member<'_>,
    name: &str,
) -> Result<Option<Box<str>>, DecodeError> {
    if is_null(member) {
        Ok(None)
    } else {
        text_member(member, name).map(Some)
    }
}

pub(super) fn opt_raw_json_member(
    member: &Member<'_>,
    name: &str,
) -> Result<Option<RawJson>, DecodeError> {
    if is_null(member) {
        return Ok(None);
    }
    let (offset, raw) = (member.offset, member.value);
    RawJson::parse(raw)
        .map(Some)
        .map_err(|error| invalid(offset, format!("member `{name}`: {error}")))
}

pub(super) fn parts_member(member: &Member<'_>) -> Result<Vec<JournalPart>, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    let mut parts = Vec::new();
    for (item_offset, item) in scan_items(raw, offset)? {
        parts.push(decode_part(item, item_offset)?);
    }
    Ok(parts)
}

pub(super) fn decode_part(value: &str, offset: usize) -> Result<JournalPart, DecodeError> {
    let mut members = object_members(
        value,
        offset,
        &["type", "text", "mime", "base64", "blob", "bytes"],
    )?;
    let ty = want(&mut members, "type")
        .map(|member| text_member(&member, "type"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a part needs a `type` member"))?;
    let has_blob = members.iter().any(|member| member.name.as_ref() == "blob");
    let text = want(&mut members, "text")
        .map(|member| text_member(&member, "text"))
        .transpose()?;
    let mime = want(&mut members, "mime")
        .map(|member| text_member(&member, "mime"))
        .transpose()?;
    let base64 = want(&mut members, "base64")
        .map(|member| text_member(&member, "base64"))
        .transpose()?;
    let blob = want(&mut members, "blob")
        .map(|member| text_member(&member, "blob"))
        .transpose()?;
    let bytes = want(&mut members, "bytes")
        .map(|member| u64_member(&member, "bytes"))
        .transpose()?;
    match (ty.as_ref(), has_blob) {
        ("text", false) => Ok(JournalPart::Text {
            text: text.ok_or_else(|| invalid(offset, "a text part needs `text`"))?,
        }),
        ("text", true) => Ok(JournalPart::TextBlob {
            blob: blob.ok_or_else(|| invalid(offset, "a text blob part needs `blob`"))?,
            bytes: bytes.ok_or_else(|| invalid(offset, "a text blob part needs `bytes`"))?,
        }),
        ("image", false) => Ok(JournalPart::Image {
            mime: mime.ok_or_else(|| invalid(offset, "an image part needs `mime`"))?,
            base64: base64.ok_or_else(|| invalid(offset, "an image part needs `base64`"))?,
        }),
        ("image", true) => Ok(JournalPart::ImageBlob {
            mime: mime.ok_or_else(|| invalid(offset, "an image blob part needs `mime`"))?,
            blob: blob.ok_or_else(|| invalid(offset, "an image blob part needs `blob`"))?,
            bytes: bytes.ok_or_else(|| invalid(offset, "an image blob part needs `bytes`"))?,
        }),
        ("blob", _) if text.is_some() || base64.is_some() => Err(invalid(
            offset,
            "a blob part accepts only `mime`, `blob`, and `bytes`",
        )),
        ("blob", _) => Ok(JournalPart::Blob {
            mime: mime.ok_or_else(|| invalid(offset, "a blob part needs `mime`"))?,
            blob: blob.ok_or_else(|| invalid(offset, "a blob part needs `blob`"))?,
            bytes: bytes.ok_or_else(|| invalid(offset, "a blob part needs `bytes`"))?,
        }),
        (kind, _) => Err(invalid(offset, format!("unknown part type `{kind}`"))),
    }
}

pub(super) fn blocks_member(member: &Member<'_>) -> Result<Vec<Block>, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    let mut blocks = Vec::new();
    for (item_offset, item) in scan_items(raw, offset)? {
        blocks.push(decode_block(item, item_offset)?);
    }
    Ok(blocks)
}

pub(super) fn decode_block(value: &str, offset: usize) -> Result<Block, DecodeError> {
    let mut members = object_members(
        value,
        offset,
        &["type", "text", "replay", "id", "name", "input"],
    )?;
    let ty = want(&mut members, "type")
        .map(|member| text_member(&member, "type"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a content block needs a `type` member"))?;
    let text = want(&mut members, "text")
        .map(|member| text_member(&member, "text"))
        .transpose()?;
    let replay = want(&mut members, "replay")
        .map(|member| opt_raw_json_member(&member, "replay"))
        .transpose()?
        .flatten();
    match ty.as_ref() {
        "text" => Ok(Block::Text {
            text: text.ok_or_else(|| invalid(offset, "a text block needs `text`"))?,
        }),
        "reasoning" => Ok(Block::Reasoning {
            text: text.ok_or_else(|| invalid(offset, "a reasoning block needs `text`"))?,
            replay: replay.ok_or_else(|| invalid(offset, "a reasoning block needs `replay`"))?,
        }),
        "tool_call" => {
            let id = want(&mut members, "id")
                .map(|member| text_member(&member, "id").map(CallId::new))
                .transpose()?
                .ok_or_else(|| invalid(offset, "a tool_call block needs `id`"))?;
            let name = want(&mut members, "name")
                .map(|member| text_member(&member, "name"))
                .transpose()?
                .ok_or_else(|| invalid(offset, "a tool_call block needs `name`"))?;
            let input = want(&mut members, "input")
                .map(|member| {
                    let (item_offset, item) = (member.offset, member.value);
                    RawJson::parse(item).map_err(|error| invalid(item_offset, error.to_string()))
                })
                .transpose()?
                .ok_or_else(|| invalid(offset, "a tool_call block needs `input`"))?;
            Ok(Block::ToolCall { id, name, input })
        }
        kind => Err(invalid(
            offset,
            format!("unknown content block type `{kind}`"),
        )),
    }
}

pub(super) fn usage_member(member: &Member<'_>) -> Result<Option<Usage>, DecodeError> {
    if is_null(member) {
        return Ok(None);
    }
    let (offset, raw) = (member.offset, member.value);
    let mut members = object_members(
        raw,
        offset,
        &[
            "input",
            "output",
            "cache_read",
            "cache_write",
            "reasoning",
            "cost_micro_usd",
        ],
    )?;
    let missing = |name: &'static str| invalid(offset, format!("usage member `{name}` is missing"));
    let input = want(&mut members, "input")
        .map(|member| u64_member(&member, "input"))
        .transpose()?
        .ok_or_else(|| missing("input"))?;
    let output = want(&mut members, "output")
        .map(|member| u64_member(&member, "output"))
        .transpose()?
        .ok_or_else(|| missing("output"))?;
    let cache_read = want(&mut members, "cache_read")
        .map(|member| u64_member(&member, "cache_read"))
        .transpose()?
        .ok_or_else(|| missing("cache_read"))?;
    let cache_write = want(&mut members, "cache_write")
        .map(|member| u64_member(&member, "cache_write"))
        .transpose()?
        .ok_or_else(|| missing("cache_write"))?;
    let reasoning = match want(&mut members, "reasoning") {
        Some(member) if is_null(&member) => None,
        Some(member) => Some(u64_member(&member, "reasoning")?),
        None => return Err(missing("reasoning")),
    };
    let cost_usd = match want(&mut members, "cost_micro_usd") {
        Some(member) if is_null(&member) => None,
        Some(member) => {
            let micros = u64_member(&member, "cost_micro_usd")?;
            // Integer micro-dollars to f64 dollars; values above 2^53
            // micro-dollars lose exactness in the float model.
            #[expect(
                clippy::cast_precision_loss,
                reason = "the float cost model is pinned by the wire surface; the journal stores integer micros"
            )]
            Some(micros as f64 / 1e6)
        }
        None => return Err(missing("cost_micro_usd")),
    };
    Ok(Some(Usage {
        input_tokens: input,
        cached_input_tokens: cache_read,
        output_tokens: output,
        reasoning_tokens: reasoning,
        cache_write_tokens: cache_write,
        cost_usd,
    }))
}

pub(super) fn owner_member(member: &Member<'_>) -> Result<Owner, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    if let Ok(literal) = sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        return match literal {
            "core" => Ok(Owner::Core),
            other => Err(invalid(offset, format!("unknown owner literal `{other}`"))),
        };
    }
    let mut inner = object_members(raw, offset, &["extension"])?;
    let extension = want(&mut inner, "extension")
        .ok_or_else(|| invalid(offset, "a `who` object needs an `extension` member"))?;
    let mut fields = object_members(extension.value, extension.offset, &["name", "origin"])?;
    let name = want(&mut fields, "name")
        .map(|member| text_member(&member, "name"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "who.extension needs `name`"))?;
    let origin = want(&mut fields, "origin")
        .map(|member| text_member(&member, "origin"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "who.extension needs `origin`"))?;
    Ok(Owner::Extension { name, origin })
}

pub(super) fn purpose_member(member: &Member<'_>) -> Result<InferredPurpose, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    let mut outer = object_members(raw, offset, &["synthetic"])?;
    let synthetic = want(&mut outer, "synthetic")
        .ok_or_else(|| invalid(offset, "a `purpose` object needs a `synthetic` member"))?;
    let mut fields = object_members(synthetic.value, synthetic.offset, &["id"])?;
    let id = want(&mut fields, "id")
        .map(|member| text_member(&member, "id"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "purpose.synthetic needs `id`"))?;
    Ok(InferredPurpose::Synthetic { id })
}

/// Reads a model record's route: either the format-1 `api` and `model`
/// pair or one `route` member, never both.
pub(super) fn model_route_members(members: &mut Members<'_>) -> Result<ModelRoute, DecodeError> {
    let route = want(members, "route");
    let api = want(members, "api");
    let model = want(members, "model");
    match (route, api, model) {
        (None, Some(api), Some(model)) => Ok(ModelRoute::Api {
            family: json_member(&api)?,
            model: model_id_member(&model)?,
        }),
        (Some(route), None, None) => route_member(&route),
        (Some(route), _, _) => Err(invalid(
            route.offset,
            "member `route` cannot appear with `api` or `model`",
        )),
        (None, None, _) => Err(invalid(0, "missing member `api`")),
        (None, Some(_), None) => Err(invalid(0, "missing member `model`")),
    }
}

/// Reads a `model` id member; an empty id cannot name a provider model.
pub(super) fn model_id_member(member: &Member<'_>) -> Result<Box<str>, DecodeError> {
    let offset = member.offset;
    let model = text_member(member, "model")?;
    if model.is_empty() {
        return Err(invalid(offset, "member `model` must not be empty"));
    }
    Ok(model)
}

/// Reads `{"synthetic":{"id":..}}` or `{"harness":{"id":..}}` and checks
/// the id against the route's closed grammar.
pub(super) fn route_member(member: &Member<'_>) -> Result<ModelRoute, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    let outer = object_members(raw, offset, &["synthetic", "harness"])?;
    let mut tags = outer.into_iter();
    let (Some(first), None) = (tags.next(), tags.next()) else {
        return Err(invalid(
            offset,
            "a `route` object needs exactly one `synthetic` or `harness` member",
        ));
    };
    let mut fields = object_members(first.value, first.offset, &["id"])?;
    let id_member = want(&mut fields, "id")
        .ok_or_else(|| invalid(first.offset, format!("route.{} needs `id`", first.name)))?;
    let id_offset = id_member.offset;
    let id = text_member(&id_member, "id")?;
    let route = if first.name.as_ref() == "synthetic" {
        ModelRoute::synthetic(id)
    } else {
        ModelRoute::harness(id)
    };
    route.map_err(|error| invalid(id_offset, error.to_string()))
}

pub(super) fn parse_u64(raw: &str) -> Option<u64> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut value: u64 = 0;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))?;
    }
    Some(value)
}

pub(super) fn parse_i64(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    let (negative, digits) = match bytes {
        [b'-', rest @ ..] => (true, rest),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: i64 = 0;
    for byte in digits {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(i64::from(*byte - b'0'))?;
    }
    Some(if negative {
        value.checked_neg()?
    } else {
        value
    })
}
