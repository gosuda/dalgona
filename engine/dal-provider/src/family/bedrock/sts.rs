//! Pure decoding of AWS STS `AssumeRole` and `AssumeRoleWithWebIdentity` XML.
//!
//! [`decode_credentials`] turns a success body into the [`AwsCredential`] the `SigV4` signer
//! consumes. [`decode_error_code`] turns an error body into a redacted [`StsErrorCode`].
//! Neither function performs I/O: the caller owns the HTTP exchange, the request signing, and
//! every decision about what a failure means.
//!
//! # Backend
//!
//! Tokenization is `quick_xml::reader::Reader<&[u8]>`, the raw pull reader: an iterative state
//! machine on the caller's frame with element depth and open names on the heap, so nesting
//! costs heap, never native stack, and no capacity is reserved before the first byte is read.
//! The raw reader performs no namespace resolution and no well-formedness checking beyond
//! matching end names, so this module owns every remaining rule:
//!
//! - **Names** are validated as `NCName`-based `QName`s (one colon, `NameStartChar` first) and
//!   resolved through this module's own namespace stack: bindings are indexed by prefix for
//!   one-lookup resolution, scoped with per-scope undo, and tracked at `usize` depth. A
//!   prefix without an active binding is a fault; so is any use or binding the XML namespace
//!   specification reserves (`xmlns` as a prefix, `xml` bound elsewhere, the reserved URIs
//!   bound to other prefixes or as the default).
//! - **Attributes** are checked for duplicate *expanded* names (two spellings that resolve to
//!   the same namespace and local name), not just duplicate spellings, and their values are
//!   normalized and entity-expanded per the XML rules before use.
//! - **References** in content expand only the predefined entities and numeric character
//!   references; there is no DTD, so no other name can exist. A reference to anything else is
//!   a fault, and a character reference is checked against the XML character production.
//! - **Declarations** are accepted only as the very first thing in the body, must carry
//!   `version` first, then `encoding`, then `standalone`, with no other pseudo-attribute, and
//!   the version must be `1.0` or `1.1`.
//! - **Structure**: comments and processing instructions are skipped (their content still
//!   validated), a `DOCTYPE` in any form is refused before its contents are interpreted, and
//!   only whitespace, comments, and processing instructions may precede the root or follow it.
//!   The whole body is consumed before credentials are returned, so a truncated body, trailing
//!   markup, or a second root never yields credentials.
//!
//! # Untrusted input
//!
//! A response body is untrusted XML.
//!
//! - **Names** are compared as expanded names, the namespace URI plus the local name. Any
//!   namespace prefix that resolves to the STS namespace is accepted.
//! - **Structure** is bound from the root down: the root must be the response element of the
//!   requested [`StsOperation`], it must hold exactly one result element of that operation,
//!   and the result must hold exactly one `Credentials` element with exactly one each of
//!   `AccessKeyId`, `SecretAccessKey`, `SessionToken`, and `Expiration`. Only direct children
//!   bind. A second `Credentials` or scalar element, or one that belongs to the other
//!   operation, is refused rather than resolved by first or last wins. Elements this module
//!   does not name are ignored at any depth, so the service may add response fields.
//! - **Scalar text** is the concatenation of every text, `CDATA`, and reference child of the
//!   element, in order, with comments and processing instructions skipped, so a comment inside
//!   a value cannot truncate it. Nothing is trimmed or dropped: a whitespace fragment is part
//!   of the value, and a credential value that contains whitespace or a control character is
//!   then refused, because it would sign differently from how it travels in a header. A child
//!   element inside a scalar is refused. Text is borrowed from the input; only a value split
//!   by markup is joined into a new string.
//! - **Diagnostics** carry only static element names and failure classes. The reader's own
//!   errors can quote input (mismatched end tags, unrecognized references), so they are
//!   classified into [`XmlFault`] and their text is dropped. The timestamp parser's errors can
//!   quote input too and are dropped as well. No service `Message` is ever read.
//!
//! # Body bound
//!
//! The decoder applies no size, node-count, or time limit of its own, and invents none. Its
//! working memory is bounded by the body plus the open-element and namespace state of the
//! deepest element. The only non-streaming body bound in this crate is
//! `crate::http::read_body`: it refuses a declared length or a running total above
//! `crate::http::BODY_LIMIT` (16 MiB) and returns a `Vec<u8>`. A caller MUST pass a body read
//! that way, after a UTF-8 check, and MUST NOT pass bytes from an unbounded source. AWS
//! documents that the size of the returned session token is not fixed, so no STS-specific
//! size cap is asserted here.
//!
//! # Dependencies
//!
//! Tokenization uses `quick-xml` with default features off (no `encoding`, no `serialize`).
//! `jiff` parses the `Expiration` timestamp and `thiserror` derives the error types.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesDecl, BytesEnd, BytesPI, BytesRef, BytesStart, Event};
use quick_xml::name::{PrefixDeclaration, QName};
use quick_xml::reader::Reader;

use super::AwsCredential;

/// The XML namespace of every STS response element: the `xmlNamespace` trait of the STS
/// Smithy model and the `xmlns` of every example in the STS API reference.
const STS_NAMESPACE: &str = "https://sts.amazonaws.com/doc/2011-06-15/";

/// The interned id of [`STS_NAMESPACE`], preloaded by [`Namespaces::new`] so the walkers
/// compare ids without touching URI text. Id 0 is the preloaded `xml` binding.
const STS_NAMESPACE_ID: usize = 1;

/// The root element of an `awsQuery` error body.
const ERROR_RESPONSE: &str = "ErrorResponse";

/// The element of an error body that holds the error members.
const ERROR: &str = "Error";

/// The error member that names the failure.
const CODE: &str = "Code";

/// The container of the temporary credentials in a result element.
const CREDENTIALS: &str = "Credentials";

/// The access key id member of `Credentials`.
const ACCESS_KEY_ID: &str = "AccessKeyId";

/// The secret access key member of `Credentials`.
const SECRET_ACCESS_KEY: &str = "SecretAccessKey";

/// The session token member of `Credentials`.
const SESSION_TOKEN: &str = "SessionToken";

/// The expiry member of `Credentials`.
const EXPIRATION: &str = "Expiration";

/// The credential string members of `Credentials`, in the order they are validated.
const FIELDS: [&str; 4] = [ACCESS_KEY_ID, SECRET_ACCESS_KEY, SESSION_TOKEN, EXPIRATION];

/// The namespace the `xml` prefix is permanently bound to.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// The namespace the `xmlns` prefix denotes, which nothing may bind or use.
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The STS operation a response body must belong to.
///
/// A body decodes only under the operation it answers: the response and result elements are
/// named after the operation, so a body for the other operation is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StsOperation {
    /// `AssumeRole`: the response is `AssumeRoleResponse` holding `AssumeRoleResult`.
    AssumeRole,
    /// `AssumeRoleWithWebIdentity`: the response is `AssumeRoleWithWebIdentityResponse`
    /// holding `AssumeRoleWithWebIdentityResult`.
    AssumeRoleWithWebIdentity,
}

impl StsOperation {
    /// The local name of the root element of this operation's success body.
    const fn response_element(self) -> &'static str {
        match self {
            Self::AssumeRole => "AssumeRoleResponse",
            Self::AssumeRoleWithWebIdentity => "AssumeRoleWithWebIdentityResponse",
        }
    }

    /// The local name of the element under the root that holds `Credentials`.
    const fn result_element(self) -> &'static str {
        match self {
            Self::AssumeRole => "AssumeRoleResult",
            Self::AssumeRoleWithWebIdentity => "AssumeRoleWithWebIdentityResult",
        }
    }
}

/// Why the XML layer refused a body. Carries no input text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum XmlFault {
    /// The document declares a DTD, an entity, or an external identifier.
    #[error("DTD or entity declaration")]
    Dtd,
    /// A reference is undeclared, malformed, or could not be resolved.
    #[error("unresolvable entity reference")]
    EntityReference,
    /// The document violates the XML or namespace grammar.
    #[error("not well-formed")]
    Malformed,
}

impl XmlFault {
    /// Classifies a reader error and discards its text, which can echo input.
    ///
    /// The match is exhaustive on purpose: a reader upgrade that adds a variant must be
    /// classified here before it compiles.
    fn classify(error: &quick_xml::Error) -> Self {
        use quick_xml::Error as E;
        match error {
            E::Escape(_) => Self::EntityReference,
            E::IllFormed(quick_xml::errors::IllFormedError::UnclosedReference) => {
                Self::EntityReference
            }
            E::Io(_)
            | E::Syntax(_)
            | E::IllFormed(_)
            | E::InvalidAttr(_)
            | E::Encoding(_)
            | E::Namespace(_) => Self::Malformed,
        }
    }
}

/// What is wrong with the text of a scalar element. Carries no input text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ScalarProblem {
    /// The element holds no text once comments and processing instructions are skipped.
    #[error("empty")]
    Empty,
    /// The value holds whitespace or a control character, which a signed header cannot carry
    /// unchanged.
    #[error(
        "not usable as a credential value because it contains whitespace or a control character"
    )]
    WhitespaceOrControl,
    /// The value is not a timestamp with an offset.
    #[error("not a timestamp with an offset")]
    NotTimestamp,
}

/// A response body that does not decode. Every field is a static name or a failure class, so
/// no rendering of this error can carry response text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StsDecodeError {
    /// The XML layer refused the body.
    #[error("the STS response body is not acceptable XML ({fault})")]
    Xml {
        /// The failure class.
        fault: XmlFault,
    },
    /// The root element is not the expected element in the STS namespace.
    #[error("the STS response root is not an {expected} element in the STS namespace")]
    UnexpectedRoot {
        /// The local name the root had to carry.
        expected: &'static str,
    },
    /// A required element is absent.
    #[error("the STS response has no {element} element")]
    Missing {
        /// The local name of the absent element.
        element: &'static str,
    },
    /// A required element appears more than once, so no value is authoritative.
    #[error("the STS response has more than one {element} element")]
    Duplicate {
        /// The local name of the repeated element.
        element: &'static str,
    },
    /// A scalar element holds a child element.
    #[error("the STS {element} element contains nested markup")]
    NestedMarkup {
        /// The local name of the scalar element.
        element: &'static str,
    },
    /// The text of a scalar element is not acceptable.
    #[error("the STS {element} element is {problem}")]
    InvalidScalar {
        /// The local name of the scalar element.
        element: &'static str,
        /// What is wrong with its text.
        problem: ScalarProblem,
    },
}

/// The `Code` of an STS error body, by explicit mapping.
///
/// One variant per documented code spelling, named exactly as the spelling: the 18 common
/// error types and the action errors of `AssumeRole` and `AssumeRoleWithWebIdentity` in the
/// STS API reference, plus `RegionDisabledException`, the spelling the STS Smithy model gives
/// the same condition as `RegionDisabled`. The reference, model, and wire also disagree on
/// access denial (`AccessDenied` on the wire against `AccessDeniedException` in the
/// reference) and the expired web identity token (`ExpiredToken` against
/// `ExpiredTokenException`); both spellings are kept in each pair, not merged. Anything else,
/// including a documented code in another case or with padding, is [`Self::Unrecognized`],
/// which carries nothing from the body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StsErrorCode {
    /// `AccessDenied`.
    AccessDenied,
    /// `AccessDeniedException`.
    AccessDeniedException,
    /// `ExpiredTokenException`.
    ExpiredTokenException,
    /// `IncompleteSignature`.
    IncompleteSignature,
    /// `InternalFailure`.
    InternalFailure,
    /// `InvalidParameterCombination`.
    InvalidParameterCombination,
    /// `InvalidParameterValue`.
    InvalidParameterValue,
    /// `InvalidQueryParameter`.
    InvalidQueryParameter,
    /// `MalformedQueryString`.
    MalformedQueryString,
    /// `MissingAction`.
    MissingAction,
    /// `MissingAuthenticationToken`.
    MissingAuthenticationToken,
    /// `MissingParameter`.
    MissingParameter,
    /// `NotAuthorized`.
    NotAuthorized,
    /// `OptInRequired`.
    OptInRequired,
    /// `RequestExpired`.
    RequestExpired,
    /// `ServiceUnavailable`.
    ServiceUnavailable,
    /// `ThrottlingException`.
    ThrottlingException,
    /// `UnrecognizedClientException`.
    UnrecognizedClientException,
    /// `ValidationError`.
    ValidationError,
    /// `ExpiredToken`.
    ExpiredToken,
    /// `IDPCommunicationError`.
    IDPCommunicationError,
    /// `IDPRejectedClaim`.
    IDPRejectedClaim,
    /// `InvalidIdentityToken`.
    InvalidIdentityToken,
    /// `MalformedPolicyDocument`.
    MalformedPolicyDocument,
    /// `PackedPolicyTooLarge`.
    PackedPolicyTooLarge,
    /// `RegionDisabled`.
    RegionDisabled,
    /// `RegionDisabledException`.
    RegionDisabledException,
    /// A code outside the documented set. The text is deliberately not kept: an unknown code
    /// is service-chosen text and never reaches a diagnostic.
    Unrecognized,
}

impl StsErrorCode {
    /// Maps one code spelling by exact, case-sensitive comparison.
    fn from_wire(code: &str) -> Self {
        match code {
            "AccessDenied" => Self::AccessDenied,
            "AccessDeniedException" => Self::AccessDeniedException,
            "ExpiredTokenException" => Self::ExpiredTokenException,
            "IncompleteSignature" => Self::IncompleteSignature,
            "InternalFailure" => Self::InternalFailure,
            "InvalidParameterCombination" => Self::InvalidParameterCombination,
            "InvalidParameterValue" => Self::InvalidParameterValue,
            "InvalidQueryParameter" => Self::InvalidQueryParameter,
            "MalformedQueryString" => Self::MalformedQueryString,
            "MissingAction" => Self::MissingAction,
            "MissingAuthenticationToken" => Self::MissingAuthenticationToken,
            "MissingParameter" => Self::MissingParameter,
            "NotAuthorized" => Self::NotAuthorized,
            "OptInRequired" => Self::OptInRequired,
            "RequestExpired" => Self::RequestExpired,
            "ServiceUnavailable" => Self::ServiceUnavailable,
            "ThrottlingException" => Self::ThrottlingException,
            "UnrecognizedClientException" => Self::UnrecognizedClientException,
            "ValidationError" => Self::ValidationError,
            "ExpiredToken" => Self::ExpiredToken,
            "IDPCommunicationError" => Self::IDPCommunicationError,
            "IDPRejectedClaim" => Self::IDPRejectedClaim,
            "InvalidIdentityToken" => Self::InvalidIdentityToken,
            "MalformedPolicyDocument" => Self::MalformedPolicyDocument,
            "PackedPolicyTooLarge" => Self::PackedPolicyTooLarge,
            "RegionDisabled" => Self::RegionDisabled,
            "RegionDisabledException" => Self::RegionDisabledException,
            _ => Self::Unrecognized,
        }
    }
}

/// One active namespace binding: the prefix it names (`None` for the default), an id for its
/// namespace URI shared by every binding of the same URI, and the binding index that was
/// active before it, so that popping the scope restores exactly the previous state.
struct Binding {
    prefix: Option<Box<str>>,
    uri: usize,
    previous: Option<usize>,
}

/// The namespace state of one document.
///
/// Bindings are appended per declaration and indexed by prefix, so resolving a name is one
/// hash lookup regardless of how deeply the element nests, and closing an element undoes
/// exactly its own declarations. Namespace URIs are interned once per distinct value: the
/// id is what flows into resolution and duplicate checks, so a huge URI is hashed once at
/// declaration time and never again per attribute. The preloaded `xml` binding owns id 0
/// and the preloaded STS binding owns [`STS_NAMESPACE_ID`].
struct Namespaces {
    named: HashMap<Box<str>, usize>,
    default: Option<usize>,
    bindings: Vec<Binding>,
    uris: Vec<Box<str>>,
    by_uri: HashMap<Box<str>, usize>,
    scopes: Vec<usize>,
}

impl Namespaces {
    /// The state every document starts with: `xml` permanently bound, the STS namespace
    /// preloaded under [`STS_NAMESPACE_ID`], and nothing else.
    fn new() -> Self {
        Self {
            named: HashMap::from([(Box::from("xml"), 0)]),
            default: None,
            bindings: vec![
                Binding {
                    prefix: Some(Box::from("xml")),
                    uri: 0,
                    previous: None,
                },
                Binding {
                    prefix: None,
                    uri: STS_NAMESPACE_ID,
                    previous: None,
                },
            ],
            uris: vec![Box::from(XML_NAMESPACE), Box::from(STS_NAMESPACE)],
            by_uri: HashMap::from([
                (Box::from(XML_NAMESPACE), 0),
                (Box::from(STS_NAMESPACE), STS_NAMESPACE_ID),
            ]),
            scopes: vec![2],
        }
    }

    /// Opens one element scope. Declarations added until the matching [`Self::pop_scope`]
    /// belong to it.
    fn push_scope(&mut self) {
        self.scopes.push(self.bindings.len());
    }

    /// Closes the innermost scope, undoing exactly its declarations.
    fn pop_scope(&mut self) {
        let Some(base) = self.scopes.pop() else {
            return;
        };
        for binding in self.bindings.drain(base..).rev() {
            match (binding.prefix, binding.previous) {
                (Some(prefix), Some(index)) => {
                    self.named.insert(prefix, index);
                }
                (Some(prefix), None) => {
                    self.named.remove(&prefix);
                }
                (None, previous) => {
                    self.default = previous;
                }
            }
        }
    }

    /// Returns the id for a namespace URI, interning it on first sight. The URI bytes are
    /// hashed here, once per declaration, and never again per use.
    fn intern(&mut self, uri: &str) -> usize {
        if let Some(&id) = self.by_uri.get(uri) {
            id
        } else {
            let id = self.uris.len();
            self.uris.push(Box::from(uri));
            self.by_uri.insert(Box::from(uri), id);
            id
        }
    }

    /// Adds one binding from the current element's declarations.
    fn add(&mut self, prefix: Option<Box<str>>, uri: &str) {
        let uri = self.intern(uri);
        let index = self.bindings.len();
        let previous = match &prefix {
            Some(name) => self.named.insert(name.clone(), index),
            None => self.default.replace(index),
        };
        self.bindings.push(Binding {
            prefix,
            uri,
            previous,
        });
    }

    /// The active binding for a prefix, `None` for the default namespace.
    fn binding(&self, prefix: Option<&str>) -> Option<&Binding> {
        match prefix {
            Some(name) => self
                .named
                .get(name)
                .and_then(|&index| self.bindings.get(index)),
            None => self.default.and_then(|index| self.bindings.get(index)),
        }
    }

    /// The namespace URI of one interned id. Empty means the binding was reset and binds
    /// nothing.
    fn uri(&self, id: usize) -> &str {
        match self.uris.get(id) {
            Some(uri) => &uri[..],
            None => "",
        }
    }

    /// Resolves one qualified name through the active bindings. `use_default` decides whether
    /// an unqualified name takes the default namespace, as element names do and attribute
    /// names do not. The local name borrows only the buffer the name came from; the
    /// namespace is the id of the active binding, so no URI text is copied or hashed per
    /// use.
    fn resolve<'n>(
        &self,
        name: QName<'n>,
        use_default: bool,
    ) -> Result<(Option<usize>, &'n str), StsDecodeError> {
        let (local, prefix) = name.decompose();
        let namespace = match prefix {
            Some(prefix) => {
                let binding = self
                    .binding(Some(prefix.into_inner()))
                    .ok_or_else(malformed)?;
                if self.uri(binding.uri).is_empty() {
                    return Err(malformed());
                }
                Some(binding.uri)
            }
            None if use_default => match self.binding(None) {
                Some(binding) if !self.uri(binding.uri).is_empty() => Some(binding.uri),
                _ => None,
            },
            None => None,
        };
        Ok((namespace, local.into_inner()))
    }
}

/// One validated, namespace-resolved item of the document.
///
/// `'a` is the body lifetime, carried by owned or body-borrowed data. `'t` is the lender
/// lifetime of the opening tag staged inside the [`Scanner`]: element local names borrow the
/// staged tag because the reader's own accessors shorten every name to the call frame, so the
/// scanner parks each opening tag and lends its name out. A token must be fully processed
/// before the next one is read; the borrow checker enforces that.
///
/// Namespaces travel as interned ids, never as URI text: two spellings of one URI share one
/// id, and no per-attribute hashing of URI bytes ever happens.
#[derive(Debug)]
pub(crate) enum Token<'a, 't> {
    /// An opening tag, resolved. `empty` marks a self-closing tag, which opens and closes in
    /// one event.
    Element {
        /// The resolved namespace id, `None` when the name is unqualified.
        namespace: Option<usize>,
        /// The local name, lent from the staged tag.
        local: &'t str,
        /// Whether the tag was self-closing.
        empty: bool,
    },
    /// A closing tag, validated and balanced in the scope of its opening tag.
    End,
    /// A literal character-data chunk, already free of markup and references.
    Text(Cow<'a, str>),
    /// A literal `CDATA` chunk.
    CData(Cow<'a, str>),
    /// One character produced by a resolved reference.
    Char(char),
    /// A comment, validated and skippable.
    Comment,
    /// A processing instruction, validated and skippable.
    Instruction,
    /// The end of a balanced document.
    Eof,
}

/// Where the walk currently stands, as a `Copy` view of the container stack.
#[derive(Clone, Copy)]
enum Position {
    /// Outside the root: in the prolog, or after the root closed.
    Outside,
    /// Inside the response root.
    Root,
    /// Inside the result element.
    Result,
    /// Inside `Credentials`.
    Credentials,
    /// Inside an element that carries no binding role.
    Ignored,
}

/// Where the error-code walk currently stands.
#[derive(Clone, Copy)]
enum CodePosition {
    /// Outside the root: in the prolog, or after the root closed.
    Outside,
    /// Inside `ErrorResponse`.
    Root,
    /// Inside the `Error` element.
    Error,
    /// Inside an element that carries no binding role.
    Ignored,
}

/// The text accumulated for one scalar element, borrowing while the value is one chunk.
enum Accum<'a> {
    Empty,
    Borrowed(&'a str),
    Owned(String),
}

impl<'a> Accum<'a> {
    /// Appends one literal fragment, preserving a borrow while the value is a single chunk.
    fn push_fragment(&mut self, fragment: Cow<'a, str>) {
        match fragment {
            Cow::Borrowed(text) => match self {
                Self::Empty => *self = Self::Borrowed(text),
                Self::Borrowed(borrowed) => {
                    let mut owned = String::from(*borrowed);
                    owned.push_str(text);
                    *self = Self::Owned(owned);
                }
                Self::Owned(owned) => owned.push_str(text),
            },
            Cow::Owned(text) => match self {
                Self::Empty => *self = Self::Owned(text),
                Self::Borrowed(borrowed) => {
                    let mut owned = String::from(*borrowed);
                    owned.push_str(&text);
                    *self = Self::Owned(owned);
                }
                Self::Owned(owned) => owned.push_str(&text),
            },
        }
    }

    /// Appends one character from a resolved reference.
    fn push_char(&mut self, character: char) {
        match self {
            Self::Empty => *self = Self::Owned(character.to_string()),
            Self::Borrowed(borrowed) => {
                let mut owned = String::from(*borrowed);
                owned.push(character);
                *self = Self::Owned(owned);
            }
            Self::Owned(owned) => owned.push(character),
        }
    }

    /// The value so far, borrowed when it was never split.
    fn finish(self) -> Cow<'a, str> {
        match self {
            Self::Empty => Cow::Borrowed(""),
            Self::Borrowed(borrowed) => Cow::Borrowed(borrowed),
            Self::Owned(owned) => Cow::Owned(owned),
        }
    }
}

/// The lexical and namespace front end: turns a body into [`Token`]s, validating everything
/// the raw reader leaves open.
struct Scanner<'a> {
    reader: Reader<&'a [u8]>,
    namespaces: Namespaces,
    version: XmlVersion,
    depth: usize,
    /// The opening tag being lent out as the current [`Token::Element`], if any. Parking the
    /// tag here is what lets the token's local name borrow it: the reader's accessors shorten
    /// every name to the call frame.
    pending: Option<BytesStart<'a>>,
    /// Expanded names of the attributes of the element currently being read: an interned
    /// namespace id plus the local name. Local names are boxed because the reader's attribute
    /// iterator shortens keys to the call frame; ids keep the per-attribute work independent
    /// of URI length.
    attribute_names: HashSet<(Option<usize>, Box<str>)>,
    /// Whether an XML declaration has been seen; there may be at most one.
    declared: bool,
    /// Whether anything has been read; a declaration must precede everything.
    started: bool,
}

impl<'a> Scanner<'a> {
    fn new(body: &'a str) -> Self {
        // Every other check this front end relies on is already the reader default
        // (`check_end_names`, no dangling `&`, no unmatched ends, no empty expansion, no
        // trimming). Comment content is the one rule the reader leaves off by default, so it
        // is switched on here: `--` inside a comment is not well-formed XML.
        let mut reader = Reader::from_str(body);
        reader.config_mut().check_comments = true;
        Self {
            reader,
            namespaces: Namespaces::new(),
            version: XmlVersion::Implicit1_0,
            depth: 0,
            pending: None,
            attribute_names: HashSet::new(),
            declared: false,
            started: false,
        }
    }

    /// Reads and validates the next token.
    ///
    /// # Errors
    ///
    /// [`StsDecodeError::Xml`] for every lexical, namespace, or document-shape violation this
    /// front end owns.
    fn next(&mut self) -> Result<Token<'a, '_>, StsDecodeError> {
        loop {
            let event = self
                .reader
                .read_event()
                .map_err(|error| xml(XmlFault::classify(&error)))?;
            match event {
                Event::Start(start) => return self.start(start, false),
                Event::Empty(start) => return self.start(start, true),
                Event::End(end) => return self.end(&end),
                Event::Text(text) => {
                    self.started = true;
                    let text = text.into_inner();
                    validate_chars(&text)?;
                    return Ok(Token::Text(text));
                }
                Event::CData(data) => {
                    self.started = true;
                    let data = data.into_inner();
                    validate_chars(&data)?;
                    return Ok(Token::CData(data));
                }
                Event::Comment(comment) => {
                    self.started = true;
                    validate_chars(comment.as_ref())?;
                    return Ok(Token::Comment);
                }
                Event::GeneralRef(reference) => return self.reference(&reference),
                Event::PI(instruction) => {
                    self.started = true;
                    Self::process_instruction(&instruction)?;
                    return Ok(Token::Instruction);
                }
                Event::Decl(declaration) => self.process_xml_declaration(&declaration)?,
                Event::DocType(_) => return Err(dtd()),
                Event::Eof => return self.finish(),
            }
        }
    }

    /// Validates and resolves one opening tag, pushing its namespace scope. A self-closing
    /// tag pops its own scope again. The tag is parked in [`Self::pending`] so the returned
    /// local name can borrow it.
    ///
    /// Namespace declarations are processed first, over the whole tag and independent of
    /// attribute order, because they apply to the entire tag including attributes that
    /// precede them. Ordinary attributes are validated afterwards against the completed
    /// scope.
    fn start(
        &mut self,
        start: BytesStart<'a>,
        empty: bool,
    ) -> Result<Token<'a, '_>, StsDecodeError> {
        self.started = true;
        self.namespaces.push_scope();
        self.attribute_names.clear();
        if let Err(error) = self.process_declarations(&start) {
            self.namespaces.pop_scope();
            return Err(error);
        }
        if let Err(error) = self.process_members(&start) {
            self.namespaces.pop_scope();
            return Err(error);
        }
        self.pending = Some(start);
        let Some(staged) = self.pending.as_ref() else {
            self.namespaces.pop_scope();
            return Err(malformed());
        };
        let name = staged.name();
        if validate_qname(name.0).is_err()
            || name
                .prefix()
                .is_some_and(|prefix| prefix.into_inner() == "xmlns")
        {
            self.namespaces.pop_scope();
            return Err(malformed());
        }
        let (namespace, local) = match self.namespaces.resolve(name, true) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.namespaces.pop_scope();
                return Err(error);
            }
        };
        // Popping the scope here is safe for the returned token: the namespace travels as an
        // interned id into append-only tables that are never freed, and the local name borrows
        // the staged tag, not the scope.
        if empty {
            self.namespaces.pop_scope();
        } else {
            self.depth += 1;
        }
        Ok(Token::Element {
            namespace,
            local,
            empty,
        })
    }

    /// Validates one closing tag in the scope of its opening tag, then pops the scope.
    fn end(&mut self, end: &BytesEnd<'_>) -> Result<Token<'a, '_>, StsDecodeError> {
        self.started = true;
        let name = end.name();
        validate_qname(name.0)?;
        if name.0.starts_with("xmlns:") {
            return Err(malformed());
        }
        self.namespaces.resolve(name, true)?;
        if self.depth == 0 {
            return Err(malformed());
        }
        self.depth -= 1;
        self.namespaces.pop_scope();
        Ok(Token::End)
    }

    /// Processes every `xmlns` declaration of an opening tag into the current scope.
    fn process_declarations(&mut self, start: &BytesStart<'_>) -> Result<(), StsDecodeError> {
        for attribute in start.attributes() {
            let Ok(attribute) = attribute else {
                return Err(malformed());
            };
            if let Some(declaration) = QName(attribute.key.0).as_namespace_binding() {
                self.process_ns_declaration(&attribute, declaration)?;
            }
        }
        Ok(())
    }

    /// Validates every ordinary attribute of an opening tag against the completed scope:
    /// checked, normalized, expanded, and unique by expanded name.
    fn process_members(&mut self, start: &BytesStart<'_>) -> Result<(), StsDecodeError> {
        for attribute in start.attributes() {
            let Ok(attribute) = attribute else {
                return Err(malformed());
            };
            if QName(attribute.key.0).as_namespace_binding().is_none() {
                self.process_member(&attribute)?;
            }
        }
        Ok(())
    }

    /// Validates one ordinary attribute against the completed scope of its element: checked,
    /// normalized, expanded, and unique by expanded name.
    fn process_member(&mut self, attribute: &Attribute<'_>) -> Result<(), StsDecodeError> {
        validate_qname(attribute.key.0)?;
        let (namespace, local) = self.namespaces.resolve(attribute.key, false)?;
        let value = normalized(attribute, self.version)?;
        validate_chars(&value)?;
        if !self.attribute_names.insert((namespace, Box::from(local))) {
            return Err(malformed());
        }
        Ok(())
    }

    /// Validates one `xmlns` declaration and binds it in the current scope.
    fn process_ns_declaration(
        &mut self,
        attribute: &Attribute<'_>,
        declaration: PrefixDeclaration<'_>,
    ) -> Result<(), StsDecodeError> {
        let value = normalized(attribute, self.version)?;
        validate_chars(&value)?;
        match declaration {
            PrefixDeclaration::Default => {
                if value.as_ref() == XML_NAMESPACE || value.as_ref() == XMLNS_NAMESPACE {
                    return Err(malformed());
                }
                self.namespaces.add(None, &value);
            }
            PrefixDeclaration::Named(prefix) => {
                if prefix == "xmlns" || !is_ncname(prefix) {
                    return Err(malformed());
                }
                if prefix == "xml" {
                    if value.as_ref() != XML_NAMESPACE {
                        return Err(malformed());
                    }
                } else if value.as_ref() == XML_NAMESPACE || value.as_ref() == XMLNS_NAMESPACE {
                    return Err(malformed());
                }
                self.namespaces.add(Some(Box::from(prefix)), &value);
            }
        }
        Ok(())
    }

    /// Validates a processing instruction. Targets reserved for XML (`xml*`, any case) are
    /// refused; only the XML declaration may carry that name.
    fn process_instruction(instruction: &BytesPI<'_>) -> Result<(), StsDecodeError> {
        let target = instruction.target();
        validate_qname(target)?;
        if target
            .get(..3)
            .is_some_and(|head| head.eq_ignore_ascii_case("xml"))
        {
            return Err(malformed());
        }
        validate_chars(instruction.as_ref())
    }

    /// Validates the XML declaration. It must be the very first thing in the body, carry
    /// `version` first and then `encoding` and `standalone`, and nothing else.
    fn process_xml_declaration(
        &mut self,
        declaration: &BytesDecl<'a>,
    ) -> Result<(), StsDecodeError> {
        if self.declared || self.started {
            return Err(malformed());
        }
        self.declared = true;
        self.version = declaration.xml_version().map_err(|_| malformed())?;
        let start = BytesStart::from_content(declaration.as_ref(), 3);
        let mut latest = 0;
        for attribute in start.attributes() {
            let Ok(attribute) = attribute else {
                return Err(malformed());
            };
            let order = match attribute.key.0 {
                "version" => 0,
                "encoding" => 1,
                "standalone" => 2,
                _ => return Err(malformed()),
            };
            if order < latest {
                return Err(malformed());
            }
            latest = order;
            let value = normalized(&attribute, self.version)?;
            validate_chars(&value)?;
            match attribute.key.0 {
                "encoding" if value.is_empty() => return Err(malformed()),
                "standalone" if value.as_ref() != "yes" && value.as_ref() != "no" => {
                    return Err(malformed());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Resolves one reference. Only the predefined entities and valid numeric character
    /// references exist; everything else is a fault.
    fn reference(&mut self, reference: &BytesRef<'_>) -> Result<Token<'a, '_>, StsDecodeError> {
        self.started = true;
        if reference.is_char_ref() {
            let Ok(Some(character)) = reference.resolve_char_ref() else {
                return Err(entity_reference());
            };
            if !is_xml_char(character) {
                return Err(malformed());
            }
            return Ok(Token::Char(character));
        }
        let Some(expanded) = resolve_xml_entity(reference.as_ref()) else {
            return Err(entity_reference());
        };
        match expanded.chars().next() {
            Some(character) => Ok(Token::Char(character)),
            None => Err(entity_reference()),
        }
    }

    /// Ends the document. Open elements make the body truncated, never decodable.
    fn finish(&self) -> Result<Token<'a, '_>, StsDecodeError> {
        if self.depth != 0 {
            return Err(malformed());
        }
        Ok(Token::Eof)
    }
}

/// Decodes a successful `AssumeRole` or `AssumeRoleWithWebIdentity` body into the credential
/// the signer uses.
///
/// The credential carries the access key id, the secret access key, the session token, and
/// the expiry as Unix seconds. A fractional second in the expiry is truncated, so an expiry
/// after 1970 is never read as later than the service stated.
///
/// `body` must come from a bounded read; see the module documentation.
///
/// # Errors
///
/// [`StsDecodeError`] when the body is not acceptable XML, is not the success document of
/// `operation` in the STS namespace, lacks or repeats a required element, or holds a scalar
/// that is nested, empty, unusable as a credential value, or (for `Expiration`) not a
/// timestamp with an offset. No credential is returned for a body that fails any check.
pub(crate) fn decode_credentials(
    body: &str,
    operation: StsOperation,
) -> Result<AwsCredential, StsDecodeError> {
    let mut scanner = Scanner::new(body);
    let mut walk = CredentialsWalk::new();
    let root_name = operation.response_element();
    let result_name = operation.result_element();
    loop {
        match scanner.next()? {
            Token::Element {
                namespace,
                local,
                empty,
            } => {
                walk.open(namespace, local, empty, root_name, result_name)?;
            }
            Token::End => walk.close(result_name)?,
            Token::Text(text) | Token::CData(text) => walk.text(text)?,
            Token::Char(character) => walk.reference(character)?,
            Token::Comment | Token::Instruction => {}
            Token::Eof => break,
        }
    }
    walk.finish()
}

/// The open-element state of the success walk: one slot per open element, newest last.
struct CredentialsWalk<'a> {
    stack: Vec<Slot<'a>>,
    root_closed: bool,
    result_seen: bool,
    credentials_seen: bool,
    seen: [bool; 4],
    values: [Option<Cow<'a, str>>; 4],
}

impl<'a> CredentialsWalk<'a> {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            root_closed: false,
            result_seen: false,
            credentials_seen: false,
            seen: [false; 4],
            values: [None, None, None, None],
        }
    }

    /// Classifies where an opening tag stands, refusing nested scalar markup.
    fn position(&self) -> Result<Position, StsDecodeError> {
        match self.stack.last() {
            None => Ok(Position::Outside),
            Some(Slot::Root) => Ok(Position::Root),
            Some(Slot::Result) => Ok(Position::Result),
            Some(Slot::Credentials) => Ok(Position::Credentials),
            Some(Slot::Field(index, _)) => Err(nested_markup(FIELDS[*index])),
            Some(Slot::Ignored) => Ok(Position::Ignored),
        }
    }

    /// Binds one opening tag.
    fn open(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
        root_name: &'static str,
        result_name: &'static str,
    ) -> Result<(), StsDecodeError> {
        match self.position()? {
            Position::Outside if self.root_closed => Err(malformed()),
            Position::Outside => self.open_root(namespace, local, empty, root_name, result_name),
            Position::Root => self.open_result(namespace, local, empty, result_name),
            Position::Result => self.open_credentials(namespace, local, empty),
            Position::Credentials => self.open_field(namespace, local, empty),
            Position::Ignored => {
                if !empty {
                    self.stack.push(Slot::Ignored);
                }
                Ok(())
            }
        }
    }

    /// Binds the root element.
    fn open_root(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
        root_name: &'static str,
        result_name: &'static str,
    ) -> Result<(), StsDecodeError> {
        if !is_sts(namespace, local, root_name) {
            return Err(unexpected_root(root_name));
        }
        if empty {
            self.root_closed = true;
            if !self.result_seen {
                return Err(missing(result_name));
            }
            return Ok(());
        }
        self.stack.push(Slot::Root);
        Ok(())
    }

    /// Binds one child of the response root.
    fn open_result(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
        result_name: &'static str,
    ) -> Result<(), StsDecodeError> {
        if !is_sts(namespace, local, result_name) {
            if !empty {
                self.stack.push(Slot::Ignored);
            }
            return Ok(());
        }
        if self.result_seen {
            return Err(duplicate(result_name));
        }
        self.result_seen = true;
        if empty {
            return Err(missing(CREDENTIALS));
        }
        self.stack.push(Slot::Result);
        Ok(())
    }

    /// Binds one child of the result element.
    fn open_credentials(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
    ) -> Result<(), StsDecodeError> {
        if !is_sts(namespace, local, CREDENTIALS) {
            if !empty {
                self.stack.push(Slot::Ignored);
            }
            return Ok(());
        }
        if self.credentials_seen {
            return Err(duplicate(CREDENTIALS));
        }
        self.credentials_seen = true;
        if empty {
            return match first_missing(self.seen) {
                Some(absent) => Err(absent),
                None => Ok(()),
            };
        }
        self.stack.push(Slot::Credentials);
        Ok(())
    }

    /// Binds one child of `Credentials`.
    fn open_field(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
    ) -> Result<(), StsDecodeError> {
        let Some(index) = field_index(namespace, local) else {
            if !empty {
                self.stack.push(Slot::Ignored);
            }
            return Ok(());
        };
        if self.seen[index] {
            return Err(duplicate(FIELDS[index]));
        }
        self.seen[index] = true;
        if empty {
            self.values[index] = Some(Cow::Borrowed(""));
            return Ok(());
        }
        self.stack.push(Slot::Field(index, Accum::Empty));
        Ok(())
    }

    /// Closes the innermost open element, finalizing one scalar or container.
    fn close(&mut self, result_name: &'static str) -> Result<(), StsDecodeError> {
        match self.stack.pop() {
            None => Err(malformed()),
            Some(Slot::Field(index, accum)) => {
                self.values[index] = Some(accum.finish());
                Ok(())
            }
            Some(Slot::Credentials) => match first_missing(self.seen) {
                Some(absent) => Err(absent),
                None => Ok(()),
            },
            Some(Slot::Result) => {
                if !self.credentials_seen {
                    return Err(missing(CREDENTIALS));
                }
                Ok(())
            }
            Some(Slot::Root) => {
                if !self.result_seen {
                    return Err(missing(result_name));
                }
                self.root_closed = true;
                Ok(())
            }
            Some(Slot::Ignored) => Ok(()),
        }
    }

    /// Feeds one text or `CDATA` chunk to the open scalar, if any.
    fn text(&mut self, text: Cow<'a, str>) -> Result<(), StsDecodeError> {
        if let Some(Slot::Field(_, accum)) = self.stack.last_mut() {
            accum.push_fragment(text);
            return Ok(());
        }
        if self.stack.is_empty() {
            return require_whitespace(&text);
        }
        Ok(())
    }

    /// Feeds one resolved reference character to the open scalar, if any.
    fn reference(&mut self, character: char) -> Result<(), StsDecodeError> {
        if let Some(Slot::Field(_, accum)) = self.stack.last_mut() {
            accum.push_char(character);
            return Ok(());
        }
        if self.stack.is_empty() {
            return Err(malformed());
        }
        Ok(())
    }

    /// Builds the credential from the walked document.
    fn finish(self) -> Result<AwsCredential, StsDecodeError> {
        if !self.root_closed {
            return Err(malformed());
        }
        let [access, secret, token, expiry] = self.values;
        Ok(AwsCredential {
            access_key_id: credential_value(take(access, ACCESS_KEY_ID)?, ACCESS_KEY_ID)?,
            secret_access_key: credential_value(
                take(secret, SECRET_ACCESS_KEY)?,
                SECRET_ACCESS_KEY,
            )?,
            session_token: Some(credential_value(
                take(token, SESSION_TOKEN)?,
                SESSION_TOKEN,
            )?),
            expires_at: Some(expire_at(&take(expiry, EXPIRATION)?)?),
        })
    }
}

/// Decodes the `Code` of an STS error body.
///
/// Only the `Code` is read. The `Type`, the `Message`, and the request id are never inspected:
/// the service `Message` is free text the caller does not control, so it has no path into any
/// value or diagnostic here. A code outside the documented set decodes to
/// [`StsErrorCode::Unrecognized`].
///
/// `body` must come from a bounded read; see the module documentation.
///
/// # Errors
///
/// [`StsDecodeError`] when the body is not acceptable XML, is not an `ErrorResponse` in the
/// STS namespace, lacks or repeats `Error` or its `Code`, or the `Code` is nested or empty.
pub(crate) fn decode_error_code(body: &str) -> Result<StsErrorCode, StsDecodeError> {
    let mut scanner = Scanner::new(body);
    let mut walk = ErrorWalk::new();
    loop {
        match scanner.next()? {
            Token::Element {
                namespace,
                local,
                empty,
            } => {
                walk.open(namespace, local, empty)?;
            }
            Token::End => walk.close()?,
            Token::Text(text) | Token::CData(text) => walk.text(text)?,
            Token::Char(character) => walk.reference(character)?,
            Token::Comment | Token::Instruction => {}
            Token::Eof => break,
        }
    }
    walk.finish()
}

/// The open-element state of the error-code walk: one slot per open element, newest last.
struct ErrorWalk<'a> {
    stack: Vec<CodeSlot<'a>>,
    root_closed: bool,
    error_seen: bool,
    code_seen: bool,
    code: Option<Cow<'a, str>>,
}

impl<'a> ErrorWalk<'a> {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            root_closed: false,
            error_seen: false,
            code_seen: false,
            code: None,
        }
    }

    /// Classifies where an opening tag stands, refusing nested code markup.
    fn position(&self) -> Result<CodePosition, StsDecodeError> {
        match self.stack.last() {
            None => Ok(CodePosition::Outside),
            Some(CodeSlot::Root) => Ok(CodePosition::Root),
            Some(CodeSlot::Error) => Ok(CodePosition::Error),
            Some(CodeSlot::Code(_)) => Err(nested_markup(CODE)),
            Some(CodeSlot::Ignored) => Ok(CodePosition::Ignored),
        }
    }

    /// Binds one opening tag.
    fn open(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
    ) -> Result<(), StsDecodeError> {
        match self.position()? {
            CodePosition::Outside if self.root_closed => Err(malformed()),
            CodePosition::Outside => self.open_root(namespace, local, empty),
            CodePosition::Root => self.open_error(namespace, local, empty),
            CodePosition::Error => self.open_code(namespace, local, empty),
            CodePosition::Ignored => {
                if !empty {
                    self.stack.push(CodeSlot::Ignored);
                }
                Ok(())
            }
        }
    }

    /// Binds the error response root.
    fn open_root(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
    ) -> Result<(), StsDecodeError> {
        if !is_sts(namespace, local, ERROR_RESPONSE) {
            return Err(unexpected_root(ERROR_RESPONSE));
        }
        if empty {
            return Err(missing(ERROR));
        }
        self.stack.push(CodeSlot::Root);
        Ok(())
    }

    /// Binds one child of the error response root.
    fn open_error(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
    ) -> Result<(), StsDecodeError> {
        if !is_sts(namespace, local, ERROR) {
            if !empty {
                self.stack.push(CodeSlot::Ignored);
            }
            return Ok(());
        }
        if self.error_seen {
            return Err(duplicate(ERROR));
        }
        self.error_seen = true;
        if empty {
            return Err(missing(CODE));
        }
        self.stack.push(CodeSlot::Error);
        Ok(())
    }

    /// Binds one child of the error element.
    fn open_code(
        &mut self,
        namespace: Option<usize>,
        local: &str,
        empty: bool,
    ) -> Result<(), StsDecodeError> {
        if !is_sts(namespace, local, CODE) {
            if !empty {
                self.stack.push(CodeSlot::Ignored);
            }
            return Ok(());
        }
        if self.code_seen {
            return Err(duplicate(CODE));
        }
        self.code_seen = true;
        if empty {
            self.code = Some(Cow::Borrowed(""));
            return Ok(());
        }
        self.stack.push(CodeSlot::Code(Accum::Empty));
        Ok(())
    }

    /// Closes the innermost open element, finalizing the code member.
    fn close(&mut self) -> Result<(), StsDecodeError> {
        match self.stack.pop() {
            None => Err(malformed()),
            Some(CodeSlot::Code(accum)) => {
                self.code = Some(accum.finish());
                Ok(())
            }
            Some(CodeSlot::Error | CodeSlot::Ignored) => Ok(()),
            Some(CodeSlot::Root) => {
                self.root_closed = true;
                Ok(())
            }
        }
    }

    /// Feeds one text or `CDATA` chunk to the open code element, if any.
    fn text(&mut self, text: Cow<'a, str>) -> Result<(), StsDecodeError> {
        if let Some(CodeSlot::Code(accum)) = self.stack.last_mut() {
            accum.push_fragment(text);
            return Ok(());
        }
        if self.stack.is_empty() {
            return require_whitespace(&text);
        }
        Ok(())
    }

    /// Feeds one resolved reference character to the open code element, if any.
    fn reference(&mut self, character: char) -> Result<(), StsDecodeError> {
        if let Some(CodeSlot::Code(accum)) = self.stack.last_mut() {
            accum.push_char(character);
            return Ok(());
        }
        if self.stack.is_empty() {
            return Err(malformed());
        }
        Ok(())
    }

    /// Decodes the walked code member.
    fn finish(self) -> Result<StsErrorCode, StsDecodeError> {
        if !self.root_closed {
            return Err(malformed());
        }
        let Some(code) = self.code else {
            return Err(missing(CODE));
        };
        if code.is_empty() {
            return Err(invalid_scalar(CODE, ScalarProblem::Empty));
        }
        Ok(StsErrorCode::from_wire(&code))
    }
}

/// The container the success walk is currently inside.
enum Slot<'a> {
    /// Inside the response root, looking for the result element.
    Root,
    /// Inside the result element, looking for `Credentials`.
    Result,
    /// Inside `Credentials`, collecting the scalar members.
    Credentials,
    /// Inside one scalar member, accumulating its text.
    Field(usize, Accum<'a>),
    /// Inside an element that carries no binding role; validated but ignored.
    Ignored,
}

/// The container the error-code walk is currently inside.
enum CodeSlot<'a> {
    /// Inside `ErrorResponse`, looking for the `Error` element.
    Root,
    /// Inside `Error`, looking for the `Code` element.
    Error,
    /// Inside `Code`, accumulating its text.
    Code(Accum<'a>),
    /// Inside an element that carries no binding role; validated but ignored.
    Ignored,
}

/// Whether the resolved name is the STS-namespaced element `expected`: one id comparison,
/// independent of URI length.
fn is_sts(namespace: Option<usize>, local: &str, expected: &str) -> bool {
    local == expected && namespace == Some(STS_NAMESPACE_ID)
}

/// The index of the STS-namespaced scalar field `local`, if it is one.
fn field_index(namespace: Option<usize>, local: &str) -> Option<usize> {
    FIELDS
        .iter()
        .position(|field| is_sts(namespace, local, field))
}

/// Whether `text` is empty or XML whitespace only; anything else is content outside markup.
fn require_whitespace(text: &str) -> Result<(), StsDecodeError> {
    if text
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\r' | b'\n' | b'\t'))
    {
        Ok(())
    } else {
        Err(malformed())
    }
}

/// Whether the character is a valid XML 1.0 character.
const fn is_xml_char(character: char) -> bool {
    matches!(
        character as u32,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x0001_0000..=0x0010_FFFF
    )
}

/// Whether the character can start an `NCName`, per the `NameStartChar` table of Namespaces
/// in XML.
const fn is_name_start_char(character: char) -> bool {
    matches!(
        character as u32,
        0x41..=0x5A
            | 0x61..=0x7A
            | 0x5F
            | 0xC0..=0xD6
            | 0xD8..=0xF6
            | 0xF8..=0x2FF
            | 0x370..=0x37D
            | 0x37F..=0x1FFF
            | 0x200C..=0x200D
            | 0x2070..=0x218F
            | 0x2C00..=0x2FEF
            | 0x3001..=0xD7FF
            | 0xF900..=0xFDCF
            | 0xFDF0..=0xFFFD
            | 0x10000..=0xEFFFF
    )
}

/// Whether the character can continue an `NCName`.
const fn is_name_char(character: char) -> bool {
    is_name_start_char(character)
        || matches!(
            character as u32,
            0x2D | 0x2E | 0x30..=0x39 | 0xB7 | 0x300..=0x36F | 0x203F..=0x2040
        )
}

/// Whether `name` is a valid `NCName`.
fn is_ncname(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if is_name_start_char(first) => {}
        _ => return false,
    }
    characters.all(is_name_char)
}

/// Validates a qualified name: at most one colon, with a non-empty `NCName` on each side.
fn validate_qname(name: &str) -> Result<(), StsDecodeError> {
    let (prefix, local) = match name.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, name),
    };
    if let Some(prefix) = prefix
        && (prefix.is_empty() || !is_ncname(prefix))
    {
        return Err(malformed());
    }
    if local.is_empty() || !is_ncname(local) {
        return Err(malformed());
    }
    Ok(())
}

/// Validates that every character of `text` is a valid XML character.
fn validate_chars(text: &str) -> Result<(), StsDecodeError> {
    if text.chars().all(is_xml_char) {
        Ok(())
    } else {
        Err(malformed())
    }
}

/// Normalizes one attribute value per the XML rules and expands its references.
fn normalized<'a>(
    attribute: &Attribute<'a>,
    version: XmlVersion,
) -> Result<Cow<'a, str>, StsDecodeError> {
    attribute.normalized_value(version).map_err(|_| malformed())
}

/// A credential string member: non-empty, with no whitespace or control character, owned only
/// once it is valid.
fn credential_value(value: Cow<'_, str>, name: &'static str) -> Result<Box<str>, StsDecodeError> {
    if value.is_empty() {
        return Err(invalid_scalar(name, ScalarProblem::Empty));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid_scalar(name, ScalarProblem::WhitespaceOrControl));
    }
    Ok(match value {
        Cow::Borrowed(borrowed) => Box::from(borrowed),
        Cow::Owned(owned) => owned.into_boxed_str(),
    })
}

/// The `Expiration` member as Unix seconds.
///
/// The timestamp parser is not given the chance to describe its input: its errors echo it.
fn expire_at(value: &str) -> Result<i64, StsDecodeError> {
    if value.is_empty() {
        return Err(invalid_scalar(EXPIRATION, ScalarProblem::Empty));
    }
    value
        .parse::<Timestamp>()
        .map(Timestamp::as_second)
        .map_err(|_| invalid_scalar(EXPIRATION, ScalarProblem::NotTimestamp))
}

/// Names a collected member or reports it absent.
fn take<'a>(
    value: Option<Cow<'a, str>>,
    name: &'static str,
) -> Result<Cow<'a, str>, StsDecodeError> {
    value.ok_or_else(|| missing(name))
}

/// The error for the first absent scalar member, if any.
fn first_missing(seen: [bool; 4]) -> Option<StsDecodeError> {
    FIELDS
        .iter()
        .zip(seen)
        .find(|(_, collected)| !*collected)
        .map(|(name, _)| missing(name))
}

/// The error for a scalar element whose text is not acceptable.
const fn invalid_scalar(element: &'static str, problem: ScalarProblem) -> StsDecodeError {
    StsDecodeError::InvalidScalar { element, problem }
}

/// The error for a scalar element that holds a child element.
const fn nested_markup(element: &'static str) -> StsDecodeError {
    StsDecodeError::NestedMarkup { element }
}

/// The error for an absent element.
const fn missing(element: &'static str) -> StsDecodeError {
    StsDecodeError::Missing { element }
}

/// The error for a repeated element.
const fn duplicate(element: &'static str) -> StsDecodeError {
    StsDecodeError::Duplicate { element }
}

/// The error for a root element outside the STS namespace or the expected operation.
const fn unexpected_root(expected: &'static str) -> StsDecodeError {
    StsDecodeError::UnexpectedRoot { expected }
}

/// The XML-layer fault for a violated grammar rule.
const fn malformed() -> StsDecodeError {
    StsDecodeError::Xml {
        fault: XmlFault::Malformed,
    }
}

/// The XML-layer fault for a reference that cannot be resolved.
const fn entity_reference() -> StsDecodeError {
    StsDecodeError::Xml {
        fault: XmlFault::EntityReference,
    }
}

/// The XML-layer fault for a declared DTD.
const fn dtd() -> StsDecodeError {
    StsDecodeError::Xml {
        fault: XmlFault::Dtd,
    }
}

/// The XML-layer error for a fault class.
const fn xml(fault: XmlFault) -> StsDecodeError {
    StsDecodeError::Xml { fault }
}

#[cfg(test)]
mod tests;
