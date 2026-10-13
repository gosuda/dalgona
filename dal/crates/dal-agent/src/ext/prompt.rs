//! Prompt sections: one immutable section per extension.
//!
//! A [`PromptSection`] is either [`PromptSection::Static`] fixed bytes or
//! [`PromptSection::Session`] bytes rendered through a [`SectionFn`] once at
//! session open and again only on generation publish (plan `:3405`). The
//! builder enforces at most one section per extension
//! ([`MAX_SECTIONS_PER_EXTENSION`]); this module only carries the data.
//!
//! Render inputs ride one read-only aggregate ([`SectionCx`]): the
//! session/turn/services/caller view plus the generation snapshot id, the
//! model-visible tool list, and the resolved instruction / `SYSTEM.md`
//! content. [`SectionFn::render`] covers all of them through `cx`, so prompt
//! (`:7322`) and guard (`:8570`) consumers share one signature.

use std::sync::Arc;

use dal_core::{Gen, SessionId, TurnId, Visibility};

use super::{Caller, Services, ToolDescription};

/// Joins rendered sections with one blank line, byte-stable (plan `:3405`).
pub const SECTION_SEPARATOR: &str = "\n\n";

/// One prompt section per extension; the builder rejects the second.
pub const MAX_SECTIONS_PER_EXTENSION: usize = 1;

/// Section order, plan `:7325` product order.
///
/// Discriminant order is the render order; [`PromptOrder::position`] exposes
/// it without a second table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PromptOrder {
    /// The product identity.
    Identity,
    /// The `D2` section, rendered after the identity.
    D2,
    /// The tool list.
    Tools,
    /// The skill list.
    Skills,
    /// The rule list.
    Rules,
    /// The rulebook text.
    Rulebook,
    /// The plugin list.
    Plugins,
    /// The environment summary.
    Environment,
    /// The user instructions.
    Instructions,
    /// The fallback section.
    Fallback,
    /// The manual pointer line.
    DocsLine,
}

impl PromptOrder {
    /// Zero-based position in the `:7325` order.
    #[must_use]
    pub const fn position(self) -> u8 {
        match self {
            Self::Identity => 0,
            Self::D2 => 1,
            Self::Tools => 2,
            Self::Skills => 3,
            Self::Rules => 4,
            Self::Rulebook => 5,
            Self::Plugins => 6,
            Self::Environment => 7,
            Self::Instructions => 8,
            Self::Fallback => 9,
            Self::DocsLine => 10,
        }
    }
}

/// Read-only render context for one [`SectionFn::render`] call.
///
/// Borrowed views keep the section pure: the host owns caching (render once
/// at open, again only on generation publish). `services` is carried for
/// identity only; render calls no [`Services`] methods.
pub struct SectionCx<'a> {
    /// Host services handle of the rendering extension (identity only).
    pub services: Arc<dyn Services>,
    /// Host-minted identity of the rendering extension.
    pub caller: &'a Caller,
    /// Session being rendered.
    pub session: SessionId,
    /// Turn being rendered, when inside a turn.
    pub turn: Option<TurnId>,
    /// Generation snapshot this render belongs to.
    pub snapshot: Gen,
    /// Model-visible tool descriptions of the current generation (`:8258`).
    pub tools: &'a [ToolDescription],
    /// Resolved session instructions, when present.
    pub instructions: Option<&'a str>,
    /// Resolved `SYSTEM.md` content, when present.
    pub system_md: Option<&'a str>,
}

/// Dynamic section body.
///
/// Returns `Some` text to contribute (possibly empty, e.g. a disabled guard
/// section per `:8570`) or `None` to omit the section from the join.
pub trait SectionFn: std::fmt::Debug + Send + Sync + 'static {
    /// Render this section for `cx`.
    fn render(&self, cx: &SectionCx<'_>) -> Option<String>;
}

impl<F> SectionFn for F
where
    F: Fn(&SectionCx<'_>) -> Option<String> + std::fmt::Debug + Send + Sync + 'static,
{
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        self(cx)
    }
}

/// One immutable prompt section.
///
/// Replaces the opaque placeholder at integration. Both variants carry
/// `order` and `visibility`; construction is by value and fields are
/// read through accessors, never mutated.
#[derive(Clone, Debug)]
pub enum PromptSection {
    /// Fixed bytes contributed verbatim.
    Static {
        /// Render position in the `:7325` order.
        order: PromptOrder,
        /// Visibility gate applied by the request assembler.
        visibility: Visibility,
        /// Fixed section bytes.
        text: Box<str>,
    },
    /// Bytes rendered through `section` for each [`SectionCx`].
    Session {
        /// Render position in the `:7325` order.
        order: PromptOrder,
        /// Visibility gate applied by the request assembler.
        visibility: Visibility,
        /// Dynamic body; immutable behind `Arc`.
        section: Arc<dyn SectionFn>,
    },
}

impl PromptSection {
    /// Fixed section with model visibility.
    #[must_use]
    pub fn static_text(order: PromptOrder, text: Box<str>) -> Self {
        Self::Static {
            order,
            visibility: Visibility::Model,
            text,
        }
    }

    /// Fixed section with an explicit visibility gate.
    #[must_use]
    pub fn static_with_visibility(
        order: PromptOrder,
        visibility: Visibility,
        text: Box<str>,
    ) -> Self {
        Self::Static {
            order,
            visibility,
            text,
        }
    }

    /// Dynamic section with model visibility.
    #[must_use]
    pub fn session(order: PromptOrder, section: Arc<dyn SectionFn>) -> Self {
        Self::Session {
            order,
            visibility: Visibility::Model,
            section,
        }
    }

    /// Dynamic section with an explicit visibility gate.
    #[must_use]
    pub fn session_with_visibility(
        order: PromptOrder,
        visibility: Visibility,
        section: Arc<dyn SectionFn>,
    ) -> Self {
        Self::Session {
            order,
            visibility,
            section,
        }
    }

    /// Render position of this section.
    #[must_use]
    pub const fn order(&self) -> PromptOrder {
        match self {
            Self::Static { order, .. } | Self::Session { order, .. } => *order,
        }
    }

    /// Visibility gate of this section.
    #[must_use]
    pub const fn visibility(&self) -> Visibility {
        match self {
            Self::Static { visibility, .. } | Self::Session { visibility, .. } => *visibility,
        }
    }

    /// Render this section: fixed bytes for [`PromptSection::Static`],
    /// delegated [`SectionFn::render`] for [`PromptSection::Session`].
    #[must_use]
    pub fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        match self {
            Self::Static { text, .. } => Some(text.to_string()),
            Self::Session { section, .. } => section.render(cx),
        }
    }
}
