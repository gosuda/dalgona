//! The trusted static wire table: one provider row, several actual wires.
//!
//! A [`WireDef`] is one actual API family a built-in provider row serves for
//! a set of model ids, with its own endpoint shape and key-header style.
//! Every value here is `&'static` row data, authored once and never built
//! from a live catalog response: live metadata supplies model ids only and
//! can never redirect an endpoint, an auth style, or a header. The entry-
//! keyed lookups below are pure, allocation-free when the row keeps its
//! plain base, and the only readers of the wire table besides the adapters
//! that carry its values.
//!
//! Two row shapes materialize their default endpoint from the captured
//! environment instead of storing one: the Cloudflare rows render their
//! account ids through [`BasePart::Env`], and every Azure-routed entry
//! renders [`AZURE_ENDPOINT_TEMPLATE`]. The render happens at the same
//! bind, discovery, and request boundary that enforces the wire rules, so a
//! missing or malformed variable refuses typed before any auth or network
//! use, and an explicit table `base_url` still wins everywhere.

use std::borrow::Cow;

use dal_core::Family;

use super::config::{AuthStyle, Origin, ProviderEntry};
use crate::{EnvSnapshot, error::ProviderError, family::azure};

/// One trusted wire a built-in provider row serves for a set of model ids.
/// Immutable static data; never built from live catalog responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireDef {
    /// Which catalog model ids this wire serves.
    pub select: WireSelect,
    /// The actual adapter family (the family the resolved route carries).
    pub family: Family,
    /// This wire's endpoint, relative to the row's (possibly env-materialized) base.
    pub base: WireBase,
    /// The API-key header style for this wire. Applies only while the entry's
    /// `auth` carries its default origin (see the `Origins` record): an
    /// explicit `auth` in the entry's own table overrides every wire style.
    pub auth: AuthStyle,
}

/// Which catalog model ids one wire serves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireSelect {
    /// One exact provider-native model id.
    Exact(&'static str),
    /// A trusted id prefix (`"anthropic/"`, `"openai/"`, `"workers-ai/"`).
    /// `Exact` beats `Prefix`; the longest matching prefix wins among prefixes.
    Prefix(&'static str),
}

/// The endpoint base of one wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireBase {
    /// The row's base as configured or materialized, plus the wire's path.
    Default,
    /// A path suffix appended to the row base (`"/v1"`, `"/anthropic"`, `"/compat"`).
    /// Applies to an explicit user base too: it is a fact about the path, not the host.
    Suffix(&'static str),
    /// A full replacement base for this wire on the row's own default base
    /// (bai anthropic at `https://api.b.ai`). Never replaces an explicit
    /// `base_url`: such a route refuses typed (`ResolveError::FixedWireBase`)
    /// at resolution, before any endpoint is taken.
    Override(&'static str),
}

impl WireDef {
    /// Whether `model` selects this wire.
    #[must_use]
    pub const fn selects(&self, model: &str) -> bool {
        match self.select {
            WireSelect::Exact(id) => bytes_eq(id.as_bytes(), model.as_bytes()),
            WireSelect::Prefix(prefix) => starts_with(model, prefix),
        }
    }
}

/// One part of a static endpoint template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BasePart {
    /// Literal text.
    Lit(&'static str),
    /// A captured-environment value, constrained to one syntactic shape so a
    /// captured value can never change the scheme, host, or origin of the
    /// endpoint the row names.
    Env {
        /// The captured variable the part reads. Never read from the process
        /// environment: only from the snapshot the binary captured at start.
        name: &'static str,
        /// The syntactic shape the captured value must fit.
        shape: EnvShape,
    },
    /// The Azure `OpenAI` endpoint, captured from the environment at the lazy
    /// boundary: the verbatim captured `AZURE_OPENAI_BASE_URL` when it names
    /// a URL, else the canonical endpoint the captured
    /// `AZURE_OPENAI_RESOURCE_NAME` derives
    /// (`https://<resource>.openai.azure.com/openai/v1`). Neither variable
    /// captured refuses typed `MissingEnvVariable` naming both, before any
    /// auth or network use; a malformed value refuses typed
    /// `MalformedEnvVariable` naming the offending variable. The part is a
    /// typed either/or, not a second templating layer: the only
    /// environment-shaped default besides the Cloudflare account parts, and
    /// the one rule every Azure-routed entry shares.
    AzureEndpoint,
}

/// The synthesized endpoint template of an Azure-routed entry: the row's own
/// `base_template` when it names one (the `azure-openai-responses` row), or
/// this template for an Azure-shaped entry with none (a Chat or Responses
/// `api-key` entry a host opted into the Azure adapter). Sharing the one
/// rule keeps the built-in row and the shaped entries in agreement: `base`
/// resolves the effective template the same way for both, so a shaped entry
/// renders the same pair of variables it names in its refusal.
pub(crate) const AZURE_ENDPOINT_TEMPLATE: &[BasePart] = &[BasePart::AzureEndpoint];

/// The two captured variables the Azure endpoint template reads, in render
/// order: a full base URL first, the resource name it derives otherwise.
/// The [`ProviderError::MissingEnvVariable`] sentence for a silent
/// environment names exactly this pair.
pub(crate) const AZURE_ENV_NAMES: &str = "AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME";

/// The syntactic shape a captured-environment value must fit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvShape {
    /// Exactly one DNS label (the rule `set.rs` applies to
    /// `AZURE_OPENAI_RESOURCE_NAME`): non-empty, no `.`/`/`/`?`/`#`.
    HostLabel,
    /// One path segment: non-empty, no `/`, `?`, `#`.
    PathSegment,
}

impl EnvShape {
    /// The expected-shape wording the malformed refusal names.
    #[must_use]
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Self::HostLabel => "one DNS label",
            Self::PathSegment => "one path segment",
        }
    }
}

/// Catalog model-id aliasing for one row.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelIdAlias {
    /// The catalog ids are the vendor ids; no alias applies.
    #[default]
    None,
    /// The dot and hyphen twins of every catalog id also resolve (bai).
    DotHyphen,
}

/// Whether `entry` routes through Azure `OpenAI`: a Chat or Responses row
/// with the Azure `api-key` style or Azure settings.
///
/// The style gate matters: [`azure::build`] serves only those two families,
/// so any other family keeps its own path whatever its auth style. This is
/// the one owner of the rule: the request adapter, the binding gate, and
/// the endpoint template below all read it here, so none of them can drift
/// from the others.
#[must_use]
pub(crate) fn is_azure_entry(entry: &ProviderEntry) -> bool {
    matches!(entry.family, Family::Chat | Family::Responses)
        && (entry.auth == AuthStyle::ApiKey
            || entry.settings.api_version.is_some()
            || !entry.settings.deployments.is_empty())
}

/// Whether `stored` and `queried` are the same id up to dot/hyphen
/// positions: same length, every other byte identical. The one alias rule
/// for [`ModelIdAlias::DotHyphen`] rows, shared by the wire table and the
/// catalog listing so selection and visibility agree: the queried spelling
/// is never rewritten, only recognized.
#[must_use]
pub(crate) fn dot_hyphen_twin(stored: &str, queried: &str) -> bool {
    stored.len() == queried.len()
        && stored
            .bytes()
            .zip(queried.bytes())
            .all(|(stored, queried)| {
                stored == queried
                    || (stored == b'.' && queried == b'-')
                    || (stored == b'-' && queried == b'.')
            })
}

/// The wire this entry serves for `model`: `None` for a named entry (`def`
/// is absent), a row whose `wires` is empty, or a row whose no selector
/// matches; `Exact` first, then the twin spelling on an aliasing row, then
/// the longest matching `Prefix`.
///
/// Callers that face a row with non-empty `wires` must treat `None` as a
/// refusal (the catalog refuses `ResolveError::UnboundModel` and the provider
/// binding re-enforces it); the row default never substitutes. A named entry
/// short-circuits on `def.is_none()` and a single-wire row on the empty
/// `wires` slice, so every existing row pays one branch. The twin spelling
/// binds the same wire while the queried id is kept verbatim everywhere;
/// an id no selector or twin matches stays unbound.
#[must_use]
pub(crate) fn wire_for(entry: &ProviderEntry, model: &str) -> Option<&'static WireDef> {
    let wires = entry.def?.wires;
    if wires.is_empty() {
        return None;
    }
    if let Some(wire) = wires.iter().find(|wire| {
        matches!(wire.select, WireSelect::Exact(id) if bytes_eq(id.as_bytes(), model.as_bytes()))
    }) {
        return Some(wire);
    }
    if entry
        .def
        .is_some_and(|def| def.model_id_alias == ModelIdAlias::DotHyphen)
        && let Some(wire) = wires
            .iter()
            .find(|wire| matches!(wire.select, WireSelect::Exact(id) if dot_hyphen_twin(id, model)))
    {
        return Some(wire);
    }
    let mut best: Option<&'static WireDef> = None;
    let mut best_len = 0;
    for wire in wires {
        if let WireSelect::Prefix(prefix) = wire.select
            && starts_with(model, prefix)
            && prefix.len() > best_len
        {
            best = Some(wire);
            best_len = prefix.len();
        }
    }
    best
}

/// The entry's effective endpoint template: the row's own `base_template`
/// when it names one, the Azure synthesis for an Azure-shaped entry with
/// none, and no template otherwise.
fn effective_template(entry: &ProviderEntry) -> &'static [BasePart] {
    match entry.def.map(|def| def.base_template) {
        Some(template) if !template.is_empty() => template,
        // A Chat or Responses `api-key` entry a host opted into the Azure
        // adapter carries no template of its own, but its default base is
        // the same captured endpoint: silence there refuses typed instead
        // of leaking the key to the stored base.
        _ if is_azure_entry(entry) => AZURE_ENDPOINT_TEMPLATE,
        _ => &[],
    }
}

/// The endpoint base for one attempt: the entry's explicit `base_url` when
/// `origins.base` is `Origin::Explicit` (verbatim, a borrow); otherwise the
/// entry's effective template rendered from the captured environment, or
/// the entry's plain base when the entry has no template; then the wire's
/// `WireBase`. A template renders only while the entry keeps the row's
/// default base, and a full-replacement wire base replaces the row base
/// whole, so every existing row and every named provider borrows its base
/// with zero allocation.
///
/// # Errors
/// A template `Env` part whose captured value is missing or blank fails with
/// `ProviderError::MissingEnvVariable`; a value that violates its
/// `EnvShape` fails with `ProviderError::MalformedEnvVariable`. The Azure
/// part fails the same way, naming both variables when neither is set and
/// the offending one when a value is malformed. No path substitutes an
/// empty string or reads the process environment.
pub(crate) fn base<'e>(
    entry: &'e ProviderEntry,
    wire: Option<&'static WireDef>,
    env: &EnvSnapshot,
) -> Result<Cow<'e, str>, ProviderError> {
    let wire_base = wire.map(|wire| wire.base);
    if entry.origins.base == Origin::Explicit {
        // The user's own endpoint is authoritative and borrowed verbatim. An
        // `Override` wire never reaches this branch: resolution refuses that
        // route typed (`ResolveError::FixedWireBase`) before a base is taken.
        // An invalid resource name the entry never needs is never validated.
        return Ok(match wire_base {
            Some(WireBase::Suffix(suffix)) => {
                let row_base: &str = &entry.base_url;
                Cow::Owned(format!("{row_base}{suffix}"))
            }
            _ => Cow::Borrowed(&*entry.base_url),
        });
    }
    let template = effective_template(entry);
    let plain: Cow<'e, str> = if template.is_empty() {
        // Every existing row and every named provider: the plain base, and
        // the borrow the caller already holds.
        Cow::Borrowed(&*entry.base_url)
    } else {
        Cow::Owned(render_template(template, &entry.id, entry.family, env)?)
    };
    Ok(match wire_base {
        None | Some(WireBase::Default) => plain,
        Some(WireBase::Suffix(suffix)) => Cow::Owned(format!("{plain}{suffix}")),
        // The row's own default base is replaced whole; the template never
        // renders beside a full replacement, so the override is a borrow too.
        Some(WireBase::Override(url)) => Cow::Borrowed(url),
    })
}

/// Renders a static template in order. `Lit` parts append as-is; an `Env`
/// part appends the captured value only when it is present, non-blank, and
/// inside its shape; the Azure part appends the captured endpoint the
/// [`azure_endpoint`] rule reads. Nothing is ever read from the process
/// environment here: the caller passes the snapshot captured once at start.
fn render_template(
    template: &'static [BasePart],
    provider: &str,
    family: Family,
    env: &EnvSnapshot,
) -> Result<String, ProviderError> {
    let mut base = String::new();
    for part in template {
        match *part {
            BasePart::Lit(text) => base.push_str(text),
            BasePart::Env { name, shape } => {
                let value = match env.get(name).map(str::trim) {
                    Some(value) if !value.is_empty() => value,
                    // A missing or blank variable never becomes an empty
                    // string inside the endpoint.
                    _ => {
                        return Err(ProviderError::MissingEnvVariable {
                            provider: provider.into(),
                            variable: name,
                        });
                    }
                };
                if !shape_admits(shape, value) {
                    return Err(ProviderError::MalformedEnvVariable {
                        provider: provider.into(),
                        variable: name,
                        expected: shape.describe(),
                    });
                }
                base.push_str(value);
            }
            BasePart::AzureEndpoint => base.push_str(&azure_endpoint(provider, family, env)?),
        }
    }
    Ok(base)
}

/// The Azure `OpenAI` endpoint the captured environment names: the verbatim
/// captured `AZURE_OPENAI_BASE_URL` when it names a URL, else the canonical
/// endpoint the captured `AZURE_OPENAI_RESOURCE_NAME` derives. The verbatim
/// URL passes the same admission as an explicit table base, so the
/// environment meets no weaker rule; the resource name passes the family's
/// label rule, so it can never redirect the key to another host.
///
/// # Errors
///
/// `MissingEnvVariable` naming both variables when neither is captured, and
/// `MalformedEnvVariable` naming the offending variable when a captured
/// value fails its rule. A blank variable is an unset one: nothing renders
/// empty, and an explicit table base never reaches this function.
fn azure_endpoint(
    provider: &str,
    family: Family,
    env: &EnvSnapshot,
) -> Result<String, ProviderError> {
    let set = |name: &'static str| {
        env.get(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    if let Some(base) = set(azure::BASE_URL_ENV) {
        if crate::http::check_base_url(family, base).is_err() {
            return Err(ProviderError::MalformedEnvVariable {
                provider: provider.into(),
                variable: azure::BASE_URL_ENV,
                expected: "an absolute https URL",
            });
        }
        return Ok(base.to_owned());
    }
    if let Some(resource) = set(azure::RESOURCE_NAME_ENV) {
        return azure::base_url_for_resource(resource).map_err(|_| {
            ProviderError::MalformedEnvVariable {
                provider: provider.into(),
                variable: azure::RESOURCE_NAME_ENV,
                expected: "one DNS label",
            }
        });
    }
    Err(ProviderError::MissingEnvVariable {
        provider: provider.into(),
        variable: AZURE_ENV_NAMES,
    })
}

/// Whether `value` fits `shape`. The shared rule first: a captured value is
/// one printable ASCII token, so no space, control character, or non-ASCII
/// byte can split the request line or smuggle a header. `HostLabel` then
/// refuses every character that can leave one DNS label (`.` starts another
/// label; `/`, `?`, `#` end the host; `:`/`@`/`%`/`\` change the authority),
/// and `PathSegment` refuses the characters that end or escape one segment
/// and the dot segments that climb.
fn shape_admits(shape: EnvShape, value: &str) -> bool {
    if value.is_empty() || value == "." || value == ".." {
        return false;
    }
    for c in value.chars() {
        if !c.is_ascii_graphic() {
            return false;
        }
        match shape {
            EnvShape::HostLabel if matches!(c, '.' | '/' | '?' | '#' | ':' | '@' | '%' | '\\') => {
                return false;
            }
            EnvShape::PathSegment if matches!(c, '/' | '?' | '#' | '%' | '\\') => return false,
            _ => {}
        }
    }
    true
}

/// Byte equality, so `WireDef::selects` stays a `const fn`.
const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Whether `haystack` starts with `needle`, so `WireDef::selects` stays a
/// `const fn`.
const fn starts_with(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    if haystack.len() < needle.len() {
        return false;
    }
    let mut index = 0;
    while index < needle.len() {
        if haystack[index] != needle[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::{ProviderDef, config::Origins};
    use super::*;

    const GATEWAY_TEMPLATE: &[BasePart] = &[
        BasePart::Lit("https://gateway.ai.cloudflare.com/v1/"),
        BasePart::Env {
            name: "CLOUDFLARE_ACCOUNT_ID",
            shape: EnvShape::HostLabel,
        },
        BasePart::Lit("/"),
        BasePart::Env {
            name: "CLOUDFLARE_GATEWAY_ID",
            shape: EnvShape::PathSegment,
        },
    ];

    const GATEWAY_WIRES: &[WireDef] = &[
        WireDef {
            select: WireSelect::Prefix("anthropic/"),
            family: Family::Anthropic,
            base: WireBase::Suffix("/anthropic"),
            auth: AuthStyle::CfAigAuthorization,
        },
        WireDef {
            select: WireSelect::Prefix("openai/"),
            family: Family::Responses,
            base: WireBase::Suffix("/openai"),
            auth: AuthStyle::CfAigAuthorization,
        },
        WireDef {
            select: WireSelect::Prefix("workers-ai/"),
            family: Family::Chat,
            base: WireBase::Suffix("/compat"),
            auth: AuthStyle::CfAigAuthorization,
        },
    ];

    const BAI_WIRES: &[WireDef] = &[WireDef {
        select: WireSelect::Exact("claude-opus-5"),
        family: Family::Anthropic,
        base: WireBase::Override("https://api.b.ai"),
        auth: AuthStyle::Bearer,
    }];

    const PLAIN_TEMPLATE: &[BasePart] = &[];
    static SUFFIX_WIRE: WireDef = WireDef {
        select: WireSelect::Prefix("x/"),
        family: Family::Chat,
        base: WireBase::Suffix("/v1"),
        auth: AuthStyle::Bearer,
    };

    static CHAT_WIRE: WireDef = WireDef {
        select: WireSelect::Prefix("gpt/"),
        family: Family::Chat,
        base: WireBase::Default,
        auth: AuthStyle::Bearer,
    };

    /// A row in the shape of the cloudflare-ai-gateway vendor row. A static
    /// gives the def the stable `'static` address `ProviderEntry.def` needs.
    static GATEWAY_DEF: ProviderDef = ProviderDef {
        id: "cloudflare-ai-gateway",
        name: "Cloudflare AI Gateway",
        family: Family::Chat,
        family_policy: super::super::def::FamilyPolicy::Native,
        base_url: "https://gateway-row.example.test",
        transport: super::super::Transport::Https,
        key: None,
        oauth: None,
        shape: super::super::Shape::of_family(Family::Chat),
        base_template: GATEWAY_TEMPLATE,
        wires: GATEWAY_WIRES,
        session_header: None,
        model_id_alias: ModelIdAlias::None,
    };

    static PLAIN_DEF: ProviderDef = ProviderDef {
        id: "openai",
        name: "OpenAI",
        family: Family::Chat,
        family_policy: super::super::def::FamilyPolicy::Native,
        base_url: "https://api.openai.com/v1",
        transport: super::super::Transport::Https,
        key: None,
        oauth: None,
        shape: super::super::Shape::of_family(Family::Chat),
        base_template: PLAIN_TEMPLATE,
        wires: &[],
        session_header: None,
        model_id_alias: ModelIdAlias::None,
    };

    static EXACT_WIRES: &[WireDef] = &[
        GATEWAY_WIRES[0],
        WireDef {
            select: WireSelect::Exact("anthropic/claude-opus-5"),
            family: Family::Anthropic,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
    ];

    static EXACT_DEF: ProviderDef = ProviderDef {
        id: "cloudflare-ai-gateway",
        name: "Cloudflare AI Gateway",
        family: Family::Chat,
        family_policy: super::super::def::FamilyPolicy::Native,
        base_url: "https://gateway-row.example.test",
        transport: super::super::Transport::Https,
        key: None,
        oauth: None,
        shape: super::super::Shape::of_family(Family::Chat),
        base_template: PLAIN_TEMPLATE,
        wires: EXACT_WIRES,
        session_header: None,
        model_id_alias: ModelIdAlias::None,
    };

    static OVERRIDE_DEF: ProviderDef = ProviderDef {
        id: "cloudflare-ai-gateway",
        name: "Cloudflare AI Gateway",
        family: Family::Chat,
        family_policy: super::super::def::FamilyPolicy::Native,
        base_url: "https://gateway-row.example.test",
        transport: super::super::Transport::Https,
        key: None,
        oauth: None,
        shape: super::super::Shape::of_family(Family::Chat),
        base_template: PLAIN_TEMPLATE,
        wires: BAI_WIRES,
        session_header: None,
        model_id_alias: ModelIdAlias::None,
    };

    static ANCHORED_WIRES: &[WireDef] = &[
        WireDef {
            select: WireSelect::Exact("a.b"),
            family: Family::Chat,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
        WireDef {
            select: WireSelect::Prefix("a"),
            family: Family::Responses,
            base: WireBase::Default,
            auth: AuthStyle::Bearer,
        },
    ];

    static ANCHORED_DEF: ProviderDef = ProviderDef {
        id: "cloudflare-ai-gateway",
        name: "Cloudflare AI Gateway",
        family: Family::Chat,
        family_policy: super::super::def::FamilyPolicy::Native,
        base_url: "https://gateway-row.example.test",
        transport: super::super::Transport::Https,
        key: None,
        oauth: None,
        shape: super::super::Shape::of_family(Family::Chat),
        base_template: PLAIN_TEMPLATE,
        wires: ANCHORED_WIRES,
        session_header: None,
        model_id_alias: ModelIdAlias::DotHyphen,
    };

    static PLAIN_TWIN_WIRES: &[WireDef] = &[WireDef {
        select: WireSelect::Exact("a.b"),
        family: Family::Chat,
        base: WireBase::Default,
        auth: AuthStyle::Bearer,
    }];

    static PLAIN_TWIN_DEF: ProviderDef = ProviderDef {
        id: "cloudflare-ai-gateway",
        name: "Cloudflare AI Gateway",
        family: Family::Chat,
        family_policy: super::super::def::FamilyPolicy::Native,
        base_url: "https://gateway-row.example.test",
        transport: super::super::Transport::Https,
        key: None,
        oauth: None,
        shape: super::super::Shape::of_family(Family::Chat),
        base_template: PLAIN_TEMPLATE,
        wires: PLAIN_TWIN_WIRES,
        session_header: None,
        model_id_alias: ModelIdAlias::None,
    };

    fn gateway_row() -> ProviderEntry {
        ProviderEntry {
            id: "cloudflare-ai-gateway".into(),
            def: Some(&GATEWAY_DEF),
            family: Family::Chat,
            base_url: "https://gateway-row.example.test".into(),
            transport: super::super::Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
            settings: super::super::config::ProviderSettings::default(),
            origins: Origins {
                api: Origin::RowDefault,
                base: Origin::RowDefault,
                auth: Origin::RowDefault,
            },
        }
    }

    fn plain_row() -> ProviderEntry {
        ProviderEntry {
            id: "openai".into(),
            def: Some(&PLAIN_DEF),
            family: Family::Chat,
            base_url: "https://api.openai.com/v1".into(),
            transport: super::super::Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
            settings: super::super::config::ProviderSettings::default(),
            origins: Origins {
                api: Origin::RowDefault,
                base: Origin::RowDefault,
                auth: Origin::RowDefault,
            },
        }
    }

    fn named_entry(base_url: &str) -> ProviderEntry {
        ProviderEntry {
            id: "zenmux".into(),
            def: None,
            family: Family::Chat,
            base_url: base_url.into(),
            transport: super::super::Transport::Https,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
            settings: super::super::config::ProviderSettings::default(),
            origins: Origins {
                api: Origin::Explicit,
                base: Origin::Explicit,
                auth: Origin::RowDefault,
            },
        }
    }

    #[test]
    fn exact_select_beats_prefix_and_longest_prefix_wins() {
        // Exact beats a matching prefix.
        let exact_row = ProviderEntry {
            def: Some(&EXACT_DEF),
            ..gateway_row()
        };
        let wire = wire_for(&exact_row, "anthropic/claude-opus-5").expect("the exact id binds");
        assert!(matches!(wire.select, WireSelect::Exact(_)));
        // Among prefixes, the longest wins.
        let wire = wire_for(&gateway_row(), "anthropic/claude-opus-5")
            .expect("the anthropic prefix binds");
        assert_eq!(wire.family, Family::Anthropic);
        assert_eq!(wire.base, WireBase::Suffix("/anthropic"));
    }

    #[test]
    fn an_unbound_id_returns_none_without_a_row_default_fallback() {
        assert_eq!(wire_for(&gateway_row(), "vendor/private-model"), None);
        // An id another row binds is unbound here: no row-default substitution.
        assert_eq!(wire_for(&gateway_row(), "gpt-5.2"), None);
    }

    #[test]
    fn a_named_entry_and_a_single_wire_row_short_circuit() {
        let named = named_entry("https://zenmux.example.test");
        assert_eq!(wire_for(&named, "anything"), None);
        let plain = plain_row();
        assert_eq!(wire_for(&plain, "gpt-5.2"), None);
    }

    #[test]
    fn selects_matches_exact_and_prefix_only() {
        let wire = &GATEWAY_WIRES[0];
        assert!(wire.selects("anthropic/claude-opus-5"));
        assert!(!wire.selects("openai/gpt-5.2"));
        assert!(!wire.selects("anthropic"));
        let exact = &BAI_WIRES[0];
        assert!(exact.selects("claude-opus-5"));
        assert!(!exact.selects("claude-opus-5-20260101"));
    }

    #[test]
    fn a_row_without_a_template_borrows_its_base_and_applies_the_wire_suffix() {
        let env = EnvSnapshot::test(&[]);
        let plain = plain_row();
        let borrowed_endpoint = base(&plain, None, &env).expect("a template-less row borrows");
        assert!(matches!(
            borrowed_endpoint,
            Cow::Borrowed("https://api.openai.com/v1")
        ));
        // A suffix wire appends to the borrowed base; only then does it own.
        let suffixed_endpoint = base(&plain, Some(&SUFFIX_WIRE), &env).expect("the suffix appends");
        assert_eq!(&*suffixed_endpoint, "https://api.openai.com/v1/v1");
    }

    #[test]
    fn an_explicit_base_is_borrowed_verbatim_and_a_suffix_appends_to_it() {
        let env = EnvSnapshot::test(&[]);
        let explicit = ProviderEntry {
            base_url: "https://user.example.test/proxy".into(),
            origins: Origins {
                base: Origin::Explicit,
                ..gateway_row().origins
            },
            ..gateway_row()
        };
        // No wire: the user's endpoint, borrowed.
        let verbatim_endpoint = base(&explicit, None, &env).expect("an explicit base borrows");
        assert!(matches!(
            verbatim_endpoint,
            Cow::Borrowed("https://user.example.test/proxy")
        ));
        // A suffix wire appends its path fact to the explicit base.
        let anthropic = &GATEWAY_WIRES[0];
        let suffixed_endpoint = base(&explicit, Some(anthropic), &env).expect("the suffix appends");
        assert_eq!(
            &*suffixed_endpoint,
            "https://user.example.test/proxy/anthropic"
        );
        // An override wire never replaces the explicit base; the catalog
        // refuses such a route, so the endpoint stays the user's own.
        let bai = &BAI_WIRES[0];
        let standing_endpoint = base(&explicit, Some(bai), &env).expect("the explicit base stands");
        assert!(matches!(
            standing_endpoint,
            Cow::Borrowed("https://user.example.test/proxy")
        ));
    }

    #[test]
    fn a_template_renders_the_captured_environment_in_order() {
        let env = EnvSnapshot::test(&[
            ("CLOUDFLARE_ACCOUNT_ID", " acct-1 "),
            ("CLOUDFLARE_GATEWAY_ID", "my-gateway"),
        ]);
        let gateway = gateway_row();
        let rendered_endpoint = base(&gateway, None, &env).expect("the template renders");
        assert_eq!(
            &*rendered_endpoint,
            "https://gateway.ai.cloudflare.com/v1/acct-1/my-gateway"
        );
        // The wire suffix appends to the rendered template.
        let compat_endpoint =
            base(&gateway, Some(&GATEWAY_WIRES[2]), &env).expect("the workers-ai wire appends");
        assert_eq!(
            &*compat_endpoint,
            "https://gateway.ai.cloudflare.com/v1/acct-1/my-gateway/compat"
        );
    }

    #[test]
    fn a_missing_variable_refuses_typed_and_names_the_variable() {
        let env = EnvSnapshot::test(&[]);
        let error = base(&gateway_row(), None, &env).expect_err("unset variable refuses");
        assert_eq!(
            error.to_string(),
            "provider cloudflare-ai-gateway needs CLOUDFLARE_ACCOUNT_ID; set it or give \
             providers.cloudflare-ai-gateway.base_url explicitly"
        );
        assert_eq!(
            error.fix().as_deref(),
            Some(
                "provider cloudflare-ai-gateway needs CLOUDFLARE_ACCOUNT_ID; set it or give \
                 providers.cloudflare-ai-gateway.base_url explicitly",
            ),
        );
        // A blank value is a missing value: no empty substitution.
        let env = EnvSnapshot::test(&[("CLOUDFLARE_ACCOUNT_ID", "   ")]);
        let error = base(&gateway_row(), None, &env).expect_err("blank variable refuses");
        assert!(matches!(
            error,
            ProviderError::MissingEnvVariable {
                variable: "CLOUDFLARE_ACCOUNT_ID",
                ..
            }
        ));
    }

    #[test]
    fn a_shape_violation_refuses_typed_and_names_the_expected_shape() {
        // A host label may not contain a dot, a slash, or a query.
        let env = EnvSnapshot::test(&[
            ("CLOUDFLARE_ACCOUNT_ID", "evil.example/x?y"),
            ("CLOUDFLARE_GATEWAY_ID", "gw"),
        ]);
        let error = base(&gateway_row(), None, &env).expect_err("a dotted host label refuses");
        assert!(matches!(
            error,
            ProviderError::MalformedEnvVariable {
                variable: "CLOUDFLARE_ACCOUNT_ID",
                expected: "one DNS label",
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "provider cloudflare-ai-gateway: CLOUDFLARE_ACCOUNT_ID is not one DNS label"
        );
        // A path segment may not climb, split, or query.
        let env = EnvSnapshot::test(&[
            ("CLOUDFLARE_ACCOUNT_ID", "acct"),
            ("CLOUDFLARE_GATEWAY_ID", "..%2Fadmin"),
        ]);
        let error = base(&gateway_row(), None, &env).expect_err("a traversal segment refuses");
        assert!(matches!(
            error,
            ProviderError::MalformedEnvVariable {
                variable: "CLOUDFLARE_GATEWAY_ID",
                expected: "one path segment",
                ..
            }
        ));
    }

    #[test]
    fn no_captured_value_can_inject_a_header_or_split_the_url() {
        for hostile in ["a b", "a\tb", "a\nb", "a%0dH", "acèse", "a\\b"] {
            let env = EnvSnapshot::test(&[
                ("CLOUDFLARE_ACCOUNT_ID", hostile),
                ("CLOUDFLARE_GATEWAY_ID", "gw"),
            ]);
            let error = base(&gateway_row(), None, &env)
                .expect_err("whitespace, control, non-ASCII, percent, and backslash refuse");
            assert!(
                matches!(
                    error,
                    ProviderError::MalformedEnvVariable {
                        variable: "CLOUDFLARE_ACCOUNT_ID",
                        ..
                    }
                ),
                "{hostile:?} must refuse as malformed"
            );
        }
    }

    #[test]
    fn a_named_entry_borrows_its_base() {
        let env = EnvSnapshot::test(&[]);
        let named = named_entry("https://zenmux.example.test/api");
        let named_endpoint = base(&named, None, &env).expect("a named entry borrows");
        assert!(matches!(
            named_endpoint,
            Cow::Borrowed("https://zenmux.example.test/api")
        ));
    }

    #[test]
    fn an_override_wire_replaces_only_the_row_default_base() {
        let env = EnvSnapshot::test(&[]);
        let row = ProviderEntry {
            def: Some(&OVERRIDE_DEF),
            base_url: "https://api.b.ai/v1".into(),
            ..gateway_row()
        };
        let overridden_endpoint =
            base(&row, Some(&BAI_WIRES[0]), &env).expect("the override applies");
        assert!(matches!(
            overridden_endpoint,
            Cow::Borrowed("https://api.b.ai")
        ));
        // The default wire keeps the row base.
        let default_endpoint = base(&row, Some(&CHAT_WIRE), &env).expect("the row base stands");
        assert!(matches!(
            default_endpoint,
            Cow::Borrowed("https://api.b.ai/v1")
        ));
    }

    /// An Azure-shaped entry without a template: the Chat family with the
    /// `api-key` style counts as shaped, bearer does not.
    fn azure_shaped(base_url: &str) -> ProviderEntry {
        ProviderEntry {
            id: "azure-shaped".into(),
            def: None,
            family: Family::Responses,
            base_url: base_url.into(),
            transport: super::super::Transport::Https,
            key_env: None,
            auth: AuthStyle::ApiKey,
            max_concurrent_requests: 1,
            settings: super::super::config::ProviderSettings::default(),
            origins: Origins::default(),
        }
    }

    #[test]
    fn an_azure_shaped_entry_renders_the_captured_base_url_verbatim() {
        let env = EnvSnapshot::test(&[
            ("AZURE_OPENAI_BASE_URL", "  https://gw.example.com/azure  "),
            ("AZURE_OPENAI_RESOURCE_NAME", "evil.example/x"),
        ]);
        // The full base URL wins over the resource name, trimmed: a resource
        // the entry never needs is never validated.
        let entry = azure_shaped("https://api.openai.com/v1");
        let base = base(&entry, None, &env).expect("the captured base renders");
        assert_eq!(&*base, "https://gw.example.com/azure");
    }

    #[test]
    fn an_azure_shaped_entry_derives_the_resource_endpoint() {
        let env = EnvSnapshot::test(&[
            ("AZURE_OPENAI_BASE_URL", "   "),
            ("AZURE_OPENAI_RESOURCE_NAME", " my-res "),
        ]);
        // A blank base URL variable is unset; the padded resource name is
        // trimmed into the canonical Responses endpoint.
        let entry = azure_shaped("https://api.openai.com/v1");
        let base = base(&entry, None, &env).expect("the resource name renders");
        assert_eq!(&*base, "https://my-res.openai.azure.com/openai/v1");
    }

    #[test]
    fn an_azure_silent_environment_refuses_typed_naming_both_variables() {
        let error = base(
            &azure_shaped("https://api.openai.com/v1"),
            None,
            &EnvSnapshot::test(&[]),
        )
        .expect_err("silence refuses");
        assert!(
            matches!(
                error,
                ProviderError::MissingEnvVariable { variable, .. }
                if variable == AZURE_ENV_NAMES
            ),
            "the refusal names the pair: {error:?}"
        );
        assert_eq!(
            error.fix().as_deref(),
            Some(
                "provider azure-shaped needs AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME; \
                 set it or give providers.azure-shaped.base_url explicitly"
            ),
        );
    }

    #[test]
    fn an_azure_malformed_value_refuses_typed_naming_the_variable() {
        // A non-URL base is not an absolute https URL.
        let env = EnvSnapshot::test(&[("AZURE_OPENAI_BASE_URL", "http://plain.example/x")]);
        let error = base(&azure_shaped("https://api.openai.com/v1"), None, &env)
            .expect_err("a plain-http base refuses");
        assert!(
            matches!(
                error,
                ProviderError::MalformedEnvVariable {
                    variable: "AZURE_OPENAI_BASE_URL",
                    expected: "an absolute https URL",
                    ..
                }
            ),
            "the base URL variable is named: {error:?}"
        );
        // A resource name outside one DNS label refuses the same way.
        let env = EnvSnapshot::test(&[("AZURE_OPENAI_RESOURCE_NAME", "evil.example/x")]);
        let error = base(&azure_shaped("https://api.openai.com/v1"), None, &env)
            .expect_err("a bad resource name refuses");
        assert!(
            matches!(
                error,
                ProviderError::MalformedEnvVariable {
                    variable: "AZURE_OPENAI_RESOURCE_NAME",
                    expected: "one DNS label",
                    ..
                }
            ),
            "the resource variable is named: {error:?}"
        );
    }

    #[test]
    fn an_explicit_base_skips_the_azure_environment_entirely() {
        let entry = ProviderEntry {
            base_url: "https://user.example.test/openai/v1".into(),
            origins: Origins {
                base: Origin::Explicit,
                ..azure_shaped("https://api.openai.com/v1").origins
            },
            ..azure_shaped("https://api.openai.com/v1")
        };
        // Both variables hostile at once: an explicit base never renders.
        let env = EnvSnapshot::test(&[
            ("AZURE_OPENAI_BASE_URL", "http://decoy.example/"),
            ("AZURE_OPENAI_RESOURCE_NAME", "evil.example/x"),
        ]);
        let base = base(&entry, None, &env).expect("the explicit base stands");
        assert!(matches!(
            base,
            Cow::Borrowed("https://user.example.test/openai/v1")
        ));
    }

    #[test]
    fn the_azure_row_carries_the_typed_template_and_no_fake_base() {
        let def = crate::find("azure-openai-responses").expect("the azure row exists");
        assert!(
            !def.base_template.is_empty(),
            "the row has a typed template"
        );
        assert!(
            !def.base_url.contains("invalid"),
            "no fake host in the row: {}",
            def.base_url
        );
    }

    #[test]
    fn a_dot_hyphen_twin_binds_the_same_wire_with_exact_first() {
        let anchored = ProviderEntry {
            def: Some(&ANCHORED_DEF),
            ..gateway_row()
        };
        // The exact spelling binds its own wire, not the prefix.
        let exact = wire_for(&anchored, "a.b").expect("exact binds");
        assert_eq!(exact.family, Family::Chat);
        // The twin binds the same wire, ahead of any prefix.
        let twin = wire_for(&anchored, "a-b").expect("the twin binds");
        assert_eq!(twin.family, Family::Chat);
        // Unknown ids stay unbound: the alias never guesses and no prefix
        // claims them.
        assert_eq!(wire_for(&anchored, "b-c"), None);
        // Without the alias the same twin spelling never reaches the exact
        // wire: the alias is what recognizes the twin as the exact id.
        let plain = ProviderEntry {
            def: Some(&PLAIN_TWIN_DEF),
            ..gateway_row()
        };
        assert_eq!(wire_for(&plain, "a-b"), None);
        assert!(wire_for(&plain, "a.b").is_some());
    }

    #[test]
    fn dot_hyphen_twin_matches_same_length_dot_slash_pairs_only() {
        assert!(dot_hyphen_twin("claude-fable-5.1", "claude-fable-5-1"));
        assert!(dot_hyphen_twin("grok-4.5", "grok-4-5"));
        assert!(dot_hyphen_twin("hy3", "hy3"));
        assert!(!dot_hyphen_twin("a.b", "a-c"));
        assert!(!dot_hyphen_twin("a.b", "a.bc"));
        assert!(!dot_hyphen_twin("ab", "a-b"));
    }
}
