//! Admission of skill-description letters into the first-prompt image set.
//!
//! [`letters`] is the one pure assembly: it draws every marked skill once,
//! in bytewise name order, and admits a drawn image only while both the
//! image count and the PNG byte total stay within their budgets. Every other
//! marked skill becomes a whole-description [`LetterFallback`] that carries
//! no bytes. Admitted ids are the decimal numbers `1` through `N` in
//! admission order, so a fallback never leaves a gap.

use std::sync::Arc;

use dal_core::Part;

use super::glyphs::Font;
use super::layout::{DrawError, DrawOutcome, LINE_H, SINGLE_W, draw};
use crate::skills::{RegisteredSkill, SkillRegistry};

/// Maximum number of letter images admitted into one assembly.
pub const IMAGE_BUDGET: usize = 20;
/// Maximum total PNG bytes admitted into one assembly.
pub const BYTE_BUDGET: usize = 3_000_000;

/// The MIME type of every provider-facing letter image.
const PNG_MIME: &str = "image/png";

/// Single-width glyph cell in pixels: width, then line height. The const
/// assertion ties the literal to the renderer geometry.
const CELL: [u8; 2] = [8, 16];
const _: () = assert!(SINGLE_W == 8 && LINE_H == 16);

/// One admitted letter image and the facts needed to persist it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LetterChunk {
    /// Decimal admission id, `1` through [`IMAGE_BUDGET`], no leading zeros.
    pub id: Box<str>,
    /// Name of the skill whose description was drawn.
    pub skill: Box<str>,
    /// Plugin that registered the skill.
    pub plugin: Box<str>,
    /// The exact description text that was drawn.
    pub source_text: Arc<str>,
    /// The complete PNG byte stream.
    pub png: Arc<[u8]>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Single-width glyph cell size in pixels: `[8, 16]`.
    pub cell: [u8; 2],
}

/// One marked skill whose description stays plain text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LetterFallback {
    /// Name of the skill that fell back.
    pub skill: Box<str>,
    /// Plugin that registered the skill.
    pub plugin: Box<str>,
    /// The whole description, which the prompt keeps as plain text.
    pub source_text: Arc<str>,
    /// First code point the font cannot draw, when that caused the fallback.
    pub first_undrawable: Option<u32>,
    /// Why the description was not admitted as an image.
    pub reason: FallbackReason,
}

/// Why a marked skill's description fell back to plain text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FallbackReason {
    /// The description holds a character the font cannot draw.
    Undrawable,
    /// Admitting the image would exceed the image or byte budget.
    OverBudget,
    /// The renderer failed for this description alone.
    RenderFailure,
}

/// The immutable result of one letter assembly.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LetterAssembly {
    /// Admitted images in id order.
    pub chunks: Vec<LetterChunk>,
    /// Marked skills kept as plain text, in bytewise name order.
    pub fallbacks: Vec<LetterFallback>,
}

/// Assembles the letter image set for every `letter2image` skill in
/// `registry`.
///
/// Unmarked skills never become candidates. Each candidate is drawn once.
/// An undrawable description, a per-description PNG encode failure, and a
/// drawn image that would exceed [`IMAGE_BUDGET`] or [`BYTE_BUDGET`] each
/// produce one [`LetterFallback`] and reserve nothing. Equal registry and
/// font inputs produce equal assemblies.
///
/// # Errors
/// Returns [`DrawError::Glyph`] when the font table fails to parse; that
/// failure affects every candidate, so no partial assembly is returned.
pub fn letters(registry: &SkillRegistry, font: &Font) -> Result<LetterAssembly, DrawError> {
    assemble(registry.iter(), font, BYTE_BUDGET)
}

/// Returns one provider-facing image part per chunk, in slice order.
///
/// A part carries only the exact MIME `image/png` and the PNG bytes; no id,
/// skill, plugin, digest, or other host metadata reaches the provider.
#[must_use]
pub fn image_parts(chunks: &[LetterChunk]) -> Vec<Part> {
    chunks
        .iter()
        .map(|chunk| Part::Image {
            mime: PNG_MIME.into(),
            bytes: Box::from(&*chunk.png),
        })
        .collect()
}

fn assemble<'a>(
    skills: impl IntoIterator<Item = &'a RegisteredSkill>,
    font: &Font,
    byte_budget: usize,
) -> Result<LetterAssembly, DrawError> {
    let mut candidates: Vec<&RegisteredSkill> = skills
        .into_iter()
        .filter(|skill| skill.letter2image)
        .collect();
    candidates.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));

    let mut assembly = LetterAssembly::default();
    let mut used_png_bytes = 0_usize;
    for skill in candidates {
        let source_text: Arc<str> = Arc::from(&*skill.description);
        let fallback = |first_undrawable, reason| LetterFallback {
            skill: skill.name.clone(),
            plugin: skill.plugin.clone(),
            source_text: Arc::clone(&source_text),
            first_undrawable,
            reason,
        };
        let image = match draw(font, &skill.description) {
            Ok((DrawOutcome::Drawn, Some(image))) => image,
            Ok((DrawOutcome::Fallback { first_undrawable }, _)) => {
                assembly
                    .fallbacks
                    .push(fallback(first_undrawable, FallbackReason::Undrawable));
                continue;
            }
            Ok((DrawOutcome::Drawn, None)) | Err(DrawError::Png(_)) => {
                assembly
                    .fallbacks
                    .push(fallback(None, FallbackReason::RenderFailure));
                continue;
            }
            Err(error @ DrawError::Glyph(_)) => return Err(error),
        };
        // `used_png_bytes <= byte_budget` holds, so the subtraction cannot wrap.
        let fits =
            assembly.chunks.len() < IMAGE_BUDGET && image.png.len() <= byte_budget - used_png_bytes;
        if !fits {
            assembly
                .fallbacks
                .push(fallback(None, FallbackReason::OverBudget));
            continue;
        }
        used_png_bytes += image.png.len();
        assembly.chunks.push(LetterChunk {
            id: (assembly.chunks.len() + 1).to_string().into_boxed_str(),
            skill: skill.name.clone(),
            plugin: skill.plugin.clone(),
            source_text,
            png: image.png,
            width: image.width,
            height: image.height,
            cell: CELL,
        });
    }
    Ok(assembly)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dal_core::Part;

    use super::{
        BYTE_BUDGET, FallbackReason, IMAGE_BUDGET, LetterAssembly, assemble, image_parts, letters,
    };
    use crate::letter::glyphs::Font;
    use crate::letter::layout::{DrawOutcome, draw, oracle::embedded_font};
    use crate::skills::{RegisteredSkill, SkillRegistry};

    const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

    fn skill(name: &str, description: &str, letter2image: bool) -> RegisteredSkill {
        RegisteredSkill {
            plugin: "plug".into(),
            name: name.into(),
            description: description.into(),
            body: Arc::from("body"),
            letter2image,
            mcp: None,
        }
    }

    fn marked(name: &str, description: &str) -> RegisteredSkill {
        skill(name, description, true)
    }

    fn png_of(font: &Font, text: &str) -> Arc<[u8]> {
        match draw(font, text) {
            Ok((DrawOutcome::Drawn, Some(image))) => image.png,
            other => panic!("fixture {text:?} must draw: {other:?}"),
        }
    }

    fn ids(assembly: &LetterAssembly) -> Vec<&str> {
        assembly.chunks.iter().map(|chunk| &*chunk.id).collect()
    }

    fn chunk_skills(assembly: &LetterAssembly) -> Vec<&str> {
        assembly.chunks.iter().map(|chunk| &*chunk.skill).collect()
    }

    fn fallback_facts(assembly: &LetterAssembly) -> Vec<(&str, Option<u32>, FallbackReason)> {
        assembly
            .fallbacks
            .iter()
            .map(|fallback| (&*fallback.skill, fallback.first_undrawable, fallback.reason))
            .collect()
    }

    #[test]
    fn fallback_isolation() {
        let font = embedded_font();
        // Input order is scrambled; admission must follow bytewise name order.
        let skills = [
            marked("gamma", "third text"),
            marked("beta", "smile \u{1F600} here"),
            marked("alpha", "first text"),
        ];
        let assembly = assemble(&skills, font, BYTE_BUDGET).expect("font parses");

        assert_eq!(ids(&assembly), ["1", "2"]);
        assert_eq!(chunk_skills(&assembly), ["alpha", "gamma"]);
        assert_eq!(
            fallback_facts(&assembly),
            [("beta", Some(0x1F600), FallbackReason::Undrawable)]
        );
        let fallback = &assembly.fallbacks[0];
        assert_eq!(&*fallback.source_text, "smile \u{1F600} here");
        assert_eq!(&*fallback.plugin, "plug");

        for (chunk, text) in assembly.chunks.iter().zip(["first text", "third text"]) {
            let (outcome, image) = draw(font, text).expect("font parses");
            let image = image.expect("drawable fixture");
            assert_eq!(outcome, DrawOutcome::Drawn);
            assert_eq!(chunk.png, image.png);
            assert_eq!((chunk.width, chunk.height), (image.width, image.height));
            assert_eq!(&*chunk.source_text, text);
            assert_eq!(chunk.cell, [8, 16]);
            assert!(chunk.png.starts_with(&PNG_SIGNATURE));
        }
    }

    #[test]
    fn image_count_boundary() {
        let font = embedded_font();
        // 22 drawable candidates, one undrawable past the cap, one unmarked.
        let mut skills: Vec<RegisteredSkill> = (0..22)
            .rev()
            .map(|index| marked(&format!("s{index:02}"), &format!("text {index}")))
            .collect();
        skills.push(marked("s99", "\u{1F600}"));
        skills.push(skill("s00-plain", "never drawn", false));

        let assembly = assemble(&skills, font, BYTE_BUDGET).expect("font parses");

        let expected_ids: Vec<String> = (1..=IMAGE_BUDGET).map(|id| id.to_string()).collect();
        assert_eq!(ids(&assembly), expected_ids);
        let expected_skills: Vec<String> = (0..IMAGE_BUDGET)
            .map(|index| format!("s{index:02}"))
            .collect();
        assert_eq!(chunk_skills(&assembly), expected_skills);
        assert_eq!(
            fallback_facts(&assembly),
            [
                ("s20", None, FallbackReason::OverBudget),
                ("s21", None, FallbackReason::OverBudget),
                ("s99", Some(0x1F600), FallbackReason::Undrawable),
            ]
        );
        let unmarked_seen = assembly
            .chunks
            .iter()
            .any(|chunk| &*chunk.skill == "s00-plain")
            || assembly
                .fallbacks
                .iter()
                .any(|fallback| &*fallback.skill == "s00-plain");
        assert!(
            !unmarked_seen,
            "an unmarked skill never becomes a candidate"
        );
        let total: usize = assembly.chunks.iter().map(|chunk| chunk.png.len()).sum();
        assert!(total <= BYTE_BUDGET);
    }

    #[test]
    fn byte_budget_boundary() {
        let font = embedded_font();
        let text_a = "aa";
        let text_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let text_c = "c";
        let (len_a, len_b, len_c) = (
            png_of(font, text_a).len(),
            png_of(font, text_b).len(),
            png_of(font, text_c).len(),
        );
        assert!(len_b > len_c, "fixture needs a larger middle PNG");
        let skills = [
            marked("a", text_a),
            marked("b", text_b),
            marked("c", text_c),
        ];

        // Exactly enough for a and c: b does not fit, is not reserved, and c
        // takes id 2 at the exact `<=` boundary.
        let exact = assemble(&skills, font, len_a + len_c).expect("font parses");
        assert_eq!(ids(&exact), ["1", "2"]);
        assert_eq!(chunk_skills(&exact), ["a", "c"]);
        assert_eq!(
            fallback_facts(&exact),
            [("b", None, FallbackReason::OverBudget)]
        );

        // One byte short: c no longer fits either.
        let short = assemble(&skills, font, len_a + len_c - 1).expect("font parses");
        assert_eq!(chunk_skills(&short), ["a"]);
        assert_eq!(
            fallback_facts(&short),
            [
                ("b", None, FallbackReason::OverBudget),
                ("c", None, FallbackReason::OverBudget),
            ]
        );

        // A first image that alone exceeds the budget falls back without
        // reserving anything, so the smaller later image still takes id 1.
        let first_too_big = assemble(&skills[1..], font, len_b - 1).expect("font parses");
        assert_eq!(ids(&first_too_big), ["1"]);
        assert_eq!(chunk_skills(&first_too_big), ["c"]);
        assert_eq!(
            fallback_facts(&first_too_big),
            [("b", None, FallbackReason::OverBudget)]
        );
    }

    #[test]
    fn malformed_font_fails_whole_assembly() {
        let font = Font::from_bytes_for_test(b"0041:00\n");
        let skills = [marked("a", "A")];
        assert!(assemble(&skills, &font, BYTE_BUDGET).is_err());
        // No marked candidate means no draw and an empty assembly.
        let unmarked = [skill("a", "A", false)];
        assert_eq!(
            assemble(&unmarked, &font, BYTE_BUDGET).expect("no draw"),
            LetterAssembly::default()
        );
    }

    #[test]
    fn registry_assembly_is_deterministic() {
        let font = embedded_font();
        // Each record's `plugin` must name its merge slot, or the registry
        // rejects the record as foreign.
        let owned_by = |plugin: &str, skill: RegisteredSkill| RegisteredSkill {
            plugin: plugin.into(),
            ..skill
        };
        let build = || {
            SkillRegistry::merge(&[
                (
                    "one",
                    vec![
                        owned_by("one", marked("zeta", "zeta text")),
                        owned_by("one", marked("alpha", "한글 text")),
                    ],
                ),
                (
                    "two",
                    vec![
                        owned_by("two", skill("mid", "plain", false)),
                        owned_by("two", marked("beta", "\u{1F600}")),
                    ],
                ),
            ])
            .expect("no conflicts")
        };
        let first = letters(&build(), font).expect("font parses");
        let second = letters(&build(), font).expect("font parses");
        assert_eq!(first, second);
        assert_eq!(chunk_skills(&first), ["alpha", "zeta"]);
        assert_eq!(
            first
                .chunks
                .iter()
                .map(|chunk| &*chunk.plugin)
                .collect::<Vec<_>>(),
            ["one", "one"]
        );
        assert_eq!(
            fallback_facts(&first),
            [("beta", Some(0x1F600), FallbackReason::Undrawable)]
        );
        assert_eq!(&*first.fallbacks[0].plugin, "two");
    }

    #[test]
    fn image_parts_carry_only_png_bytes() {
        let font = embedded_font();
        let skills = [marked("secret-skill", "hello"), marked("other", "world")];
        let assembly = assemble(&skills, font, BYTE_BUDGET).expect("font parses");
        let parts = image_parts(&assembly.chunks);
        assert_eq!(chunk_skills(&assembly), ["other", "secret-skill"]);
        assert_eq!(parts.len(), 2);
        for (part, chunk) in parts.iter().zip(&assembly.chunks) {
            let Part::Image { mime, bytes } = part else {
                panic!("letter parts are images only: {part:?}");
            };
            assert_eq!(&**mime, "image/png");
            assert_eq!(&**bytes, &*chunk.png);
            // The id is omitted: a one-digit id is too short to prove absence.
            for leaked in [&*chunk.skill, &*chunk.plugin, &*chunk.source_text] {
                assert!(
                    !bytes
                        .windows(leaked.len())
                        .any(|window| window == leaked.as_bytes()),
                    "PNG bytes leak {leaked:?}"
                );
            }
        }
        assert!(image_parts(&[]).is_empty());
    }
}
