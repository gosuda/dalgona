use std::fmt::Write as _;

use super::*;

// The wire contract is written out as literals here, not read from the module under test, so a
// wrong constant in the module cannot agree with itself.
const NAMESPACE: &str = "https://sts.amazonaws.com/doc/2011-06-15/";
const ACCESS: &str = "ASIAEXAMPLEKEYID0001";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9SAPBSM22wDOk4x4HIZ8j4FZTwdQW+/=";
const EXPIRY_TEXT: &str = "2026-01-15T12:00:00Z";
const EXPIRES_AT: i64 = 1_768_478_400;
const FIELDS: [&str; 4] = [
    "AccessKeyId",
    "SecretAccessKey",
    "SessionToken",
    "Expiration",
];
const STRING_FIELDS: [&str; 3] = ["AccessKeyId", "SecretAccessKey", "SessionToken"];
const BOTH: [StsOperation; 2] = [
    StsOperation::AssumeRole,
    StsOperation::AssumeRoleWithWebIdentity,
];

// The sample response of the STS API reference for `AssumeRoleWithWebIdentity`, verbatim.
const WEB_IDENTITY_SAMPLE: &str = r#"<AssumeRoleWithWebIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleWithWebIdentityResult>
    <SubjectFromWebIdentityToken>amzn1.account.AF6RHO7KZU5XRVQJGXK6HB56KR2A</SubjectFromWebIdentityToken>
    <Audience>client.5498841531868486423.1548@apps.example.com</Audience>
    <AssumedRoleUser>
      <Arn>arn:aws:sts::123456789012:assumed-role/FederatedWebIdentityRole/app1</Arn>
      <AssumedRoleId>AROACLKWSDQRAOEXAMPLE:app1</AssumedRoleId>
    </AssumedRoleUser>
    <Credentials>
      <SessionToken>AQoDYXdzEE0a8ANXXXXXXXXNO1ewxE5TijQyp+IEXAMPLE</SessionToken>
      <SecretAccessKey>wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEXAMPLEKEY</SecretAccessKey>
      <Expiration>2014-10-24T23:00:23Z</Expiration>
      <AccessKeyId>ASgeIAIOSFODNN7EXAMPLE</AccessKeyId>
    </Credentials>
    <SourceIdentity>SourceIdentityValue</SourceIdentity>
    <Provider>www.amazon.com</Provider>
  </AssumeRoleWithWebIdentityResult>
  <ResponseMetadata>
    <RequestId>ad4156e9-bce1-11e2-82e6-6b6efEXAMPLE</RequestId>
  </ResponseMetadata>
</AssumeRoleWithWebIdentityResponse>"#;

// The shape of the `AssumeRole` sample of the STS API reference, which prints the session token
// wrapped across indented lines. The access key id and the token are shortened.
const WRAPPED_TOKEN_SAMPLE: &str = r#"<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult>
  <SourceIdentity>Alice</SourceIdentity>
    <AssumedRoleUser>
      <Arn>arn:aws:sts::123456789012:assumed-role/demo/TestAR</Arn>
      <AssumedRoleId>ARO123EXAMPLE123:TestAR</AssumedRoleId>
    </AssumedRoleUser>
    <Credentials>
      <AccessKeyId>ASIAIOSFODNN7EXAMPLE</AccessKeyId>
      <SecretAccessKey>wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEXAMPLEKEY</SecretAccessKey>
      <SessionToken>
       AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9SAPBSM22wDOk4x4HIZ8j4FZTwdQW
       LWsKWHGBuFqwAeMicRXmxfpSPfIeoIYRqTflfKD8YUuwthAx7mSEI/qkPpKPi/kMcGd
      </SessionToken>
      <Expiration>2019-11-09T13:34:41Z</Expiration>
    </Credentials>
    <PackedPolicySize>6</PackedPolicySize>
  </AssumeRoleResult>
  <ResponseMetadata>
    <RequestId>c6104cbe-af31-11e0-8154-cbc7ccf896c7</RequestId>
  </ResponseMetadata>
</AssumeRoleResponse>"#;

const SPLIT_CASES: &[(&str, &str)] = &[
    ("ASIA<!--one-->EXAMPLE<!--two-->ID", "ASIAEXAMPLEID"),
    ("<!--lead-->ASIA<!--mid-->EXAMPLE<!--tail-->", "ASIAEXAMPLE"),
    ("ASIA<?audit trail?>EXAMPLE", "ASIAEXAMPLE"),
    ("ASIA<![CDATA[EXAMPLE]]>ID", "ASIAEXAMPLEID"),
    ("<![CDATA[ASIA]]><!--c--><![CDATA[EXAMPLE]]>", "ASIAEXAMPLE"),
    ("<![CDATA[]]>ASIA<!--c--><![CDATA[]]>EXAMPLE", "ASIAEXAMPLE"),
    ("AB&amp;C&#x44;&lt;", "AB&CD<"),
    ("<![CDATA[A&B<C]]>", "A&B<C"),
    ("A&#65;<!--c-->&#66;", "AAB"),
];

const WHITESPACE_CASES: &[&str] = &[
    "abc<!--a--> <!--b-->def",
    " abc",
    "abc ",
    "\n  abc\n",
    "ab\tcd",
    "ab\u{00A0}cd",
    "<!--x--> ",
    "abc<![CDATA[ ]]>def",
];

const EMPTY_CASES: &[&str] = &[
    "",
    "<![CDATA[]]>",
    "<!--only a comment-->",
    "<?pi?>",
    "<!--a--><!--b-->",
];

const NESTED_CASES: &[&str] = &[
    "abc<b>x</b>def",
    "<child/>",
    "<AccessKeyId>inner</AccessKeyId>",
];

fn names(operation: StsOperation) -> (&'static str, &'static str) {
    match operation {
        StsOperation::AssumeRole => ("AssumeRoleResponse", "AssumeRoleResult"),
        StsOperation::AssumeRoleWithWebIdentity => (
            "AssumeRoleWithWebIdentityResponse",
            "AssumeRoleWithWebIdentityResult",
        ),
    }
}

fn other(operation: StsOperation) -> StsOperation {
    match operation {
        StsOperation::AssumeRole => StsOperation::AssumeRoleWithWebIdentity,
        StsOperation::AssumeRoleWithWebIdentity => StsOperation::AssumeRole,
    }
}

fn envelope(operation: StsOperation, result_children: &str) -> String {
    let (root, result) = names(operation);
    format!(
        r#"<{root} xmlns="{NAMESPACE}"><{result}>{result_children}</{result}><ResponseMetadata><RequestId>req-1</RequestId></ResponseMetadata></{root}>"#
    )
}

fn members(values: [&str; 4]) -> String {
    let mut out = String::new();
    for (name, value) in FIELDS.into_iter().zip(values) {
        let _ = write!(out, "<{name}>{value}</{name}>");
    }
    out
}

fn credentials_from(values: [&str; 4]) -> String {
    format!("<Credentials>{}</Credentials>", members(values))
}

fn standard_credentials() -> String {
    credentials_from([ACCESS, SECRET, TOKEN, EXPIRY_TEXT])
}

fn credentials_with(field: &str, inner: &str) -> String {
    assert!(
        FIELDS.contains(&field),
        "{field} is not a credentials field"
    );
    let mut replaced = String::new();
    for (name, default) in FIELDS.into_iter().zip([ACCESS, SECRET, TOKEN, EXPIRY_TEXT]) {
        let value = if name == field { inner } else { default };
        let _ = write!(replaced, "<{name}>{value}</{name}>");
    }
    format!("<Credentials>{replaced}</Credentials>")
}

fn credentials_without(field: &str) -> String {
    let mut kept = String::new();
    for (name, value) in FIELDS
        .into_iter()
        .zip([ACCESS, SECRET, TOKEN, EXPIRY_TEXT])
        .filter(|(name, _)| *name != field)
    {
        let _ = write!(kept, "<{name}>{value}</{name}>");
    }
    format!("<Credentials>{kept}</Credentials>")
}

fn standard(operation: StsOperation) -> String {
    envelope(operation, &standard_credentials())
}

fn with_field(operation: StsOperation, field: &str, inner: &str) -> String {
    envelope(operation, &credentials_with(field, inner))
}

fn with_prolog(prolog: &str, access_key_inner: &str) -> String {
    let body = with_field(StsOperation::AssumeRole, "AccessKeyId", access_key_inner);
    format!("{prolog}{body}")
}

fn accepted(body: &str, operation: StsOperation) -> AwsCredential {
    decode_credentials(body, operation).expect("the body decodes")
}

fn refused(body: &str, operation: StsOperation) -> StsDecodeError {
    decode_credentials(body, operation).expect_err("the body is refused")
}

fn assert_standard(credential: &AwsCredential) {
    assert_eq!(credential.access_key_id.as_ref(), ACCESS);
    assert_eq!(credential.secret_access_key.as_ref(), SECRET);
    assert_eq!(credential.session_token.as_deref(), Some(TOKEN));
    assert_eq!(credential.expires_at, Some(EXPIRES_AT));
}

fn field_value<'a>(credential: &'a AwsCredential, field: &str) -> &'a str {
    match field {
        "AccessKeyId" => credential.access_key_id.as_ref(),
        "SecretAccessKey" => credential.secret_access_key.as_ref(),
        "SessionToken" => credential
            .session_token
            .as_deref()
            .expect("a decoded credential carries a session token"),
        unknown => panic!("{unknown} is not a string field"),
    }
}

fn renderings(error: &StsDecodeError) -> [String; 3] {
    [
        error.to_string(),
        format!("{error:?}"),
        format!("{error:#?}"),
    ]
}

// No rendering of an error may hold a planted marker or any value of the standard credential.
fn assert_redacted(error: &StsDecodeError, label: &str) {
    let leaked = renderings(error).iter().any(|text| {
        ["PLANTED", ACCESS, SECRET, TOKEN]
            .iter()
            .any(|forbidden| text.contains(*forbidden))
    });
    assert!(!leaked, "{label}: a rendering carries input text");
}

type Member = (&'static str, &'static str);

// Every rotation of the four members, forward and reversed.
fn member_orders() -> Vec<Vec<Member>> {
    let pairs: Vec<Member> = FIELDS
        .into_iter()
        .zip([ACCESS, SECRET, TOKEN, EXPIRY_TEXT])
        .collect();
    (0..4)
        .flat_map(|rotation| {
            let forward: Vec<Member> = pairs
                .iter()
                .cycle()
                .skip(rotation)
                .take(4)
                .copied()
                .collect();
            let backward: Vec<Member> = forward.iter().rev().copied().collect();
            [forward, backward]
        })
        .collect()
}

#[test]
fn success_bodies_decode_into_the_signing_credential() {
    for operation in BOTH {
        assert_standard(&accepted(&standard(operation), operation));
    }
}

#[test]
fn the_documented_web_identity_sample_decodes_with_members_in_service_order() {
    let credential = accepted(WEB_IDENTITY_SAMPLE, StsOperation::AssumeRoleWithWebIdentity);
    assert_eq!(&*credential.access_key_id, "ASgeIAIOSFODNN7EXAMPLE");
    assert_eq!(
        &*credential.secret_access_key,
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEXAMPLEKEY"
    );
    assert_eq!(
        credential.session_token.as_deref(),
        Some("AQoDYXdzEE0a8ANXXXXXXXXNO1ewxE5TijQyp+IEXAMPLE")
    );
    assert_eq!(credential.expires_at, Some(1_414_191_623));
    assert_eq!(
        refused(WEB_IDENTITY_SAMPLE, StsOperation::AssumeRole),
        StsDecodeError::UnexpectedRoot {
            expected: "AssumeRoleResponse"
        }
    );
}

#[test]
fn member_order_inside_credentials_does_not_matter() {
    for order in member_orders() {
        let mut shuffled = String::new();
        for (name, value) in order {
            let _ = write!(shuffled, "<{name}>{value}</{name}>");
        }
        let body = envelope(
            StsOperation::AssumeRole,
            &format!("<Credentials>{shuffled}</Credentials>"),
        );
        assert_standard(&accepted(&body, StsOperation::AssumeRole));
    }
}

#[test]
fn the_decoded_expiry_stops_signing_at_that_exact_second() {
    let credential = accepted(
        &standard(StsOperation::AssumeRole),
        StsOperation::AssumeRole,
    );
    assert!(credential.verify_usable(EXPIRES_AT - 1).is_ok());
    assert!(matches!(
        credential.verify_usable(EXPIRES_AT),
        Err(super::super::sigv4::CredentialError::Expired {
            expires_at: EXPIRES_AT
        })
    ));
}

#[test]
fn any_prefix_bound_to_the_sts_namespace_is_accepted() {
    let prefixed = format!(
        r#"<sts:AssumeRoleResponse xmlns:sts="{NAMESPACE}"><sts:AssumeRoleResult><sts:Credentials><sts:AccessKeyId>{ACCESS}</sts:AccessKeyId><sts:SecretAccessKey>{SECRET}</sts:SecretAccessKey><sts:SessionToken>{TOKEN}</sts:SessionToken><sts:Expiration>{EXPIRY_TEXT}</sts:Expiration></sts:Credentials></sts:AssumeRoleResult></sts:AssumeRoleResponse>"#
    );
    // Every element declares its own prefix or default namespace.
    let rebound = format!(
        r#"<a:AssumeRoleResponse xmlns:a="{NAMESPACE}"><b:AssumeRoleResult xmlns:b="{NAMESPACE}"><Credentials xmlns="{NAMESPACE}"><x:AccessKeyId xmlns:x="{NAMESPACE}">{ACCESS}</x:AccessKeyId><SecretAccessKey>{SECRET}</SecretAccessKey><y:SessionToken xmlns:y="{NAMESPACE}">{TOKEN}</y:SessionToken><Expiration>{EXPIRY_TEXT}</Expiration></Credentials></b:AssumeRoleResult></a:AssumeRoleResponse>"#
    );
    for body in [prefixed, rebound] {
        assert_standard(&accepted(&body, StsOperation::AssumeRole));
    }
}

#[test]
fn unknown_response_fields_are_ignored_at_every_level() {
    // The decoy `Credentials` precedes the real one inside an unknown member: a descendant
    // search with first-match-wins would return it.
    let credentials = format!(
        r#"<Credentials><FutureMember kind="new"><SessionToken>nested-decoy</SessionToken></FutureMember><AccessKeyId note="attributes are ignored">{ACCESS}</AccessKeyId><!-- a comment between members --><SecretAccessKey>{SECRET}</SecretAccessKey><SessionToken>{TOKEN}</SessionToken><Expiration>{EXPIRY_TEXT}</Expiration><FutureMarker/></Credentials>"#
    );
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}" xmlns:ext="urn:example:extension"><ext:Annotation>ignored</ext:Annotation><AssumeRoleResult><FutureResultMember><Credentials><AccessKeyId>decoy</AccessKeyId></Credentials></FutureResultMember>{credentials}<SourceIdentity>Alice</SourceIdentity><SessionTokenSize>68</SessionTokenSize></AssumeRoleResult><ResponseMetadata><RequestId>r</RequestId></ResponseMetadata><FutureTopLevel/></AssumeRoleResponse>"#
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn nested_unknown_members_do_not_disturb_decoding() {
    // Shallow on purpose: deep-nesting scale is exercised by its own tests below.
    let depth = 100;
    let nested = format!(
        "{}core{}",
        "<Future>".repeat(depth),
        "</Future>".repeat(depth)
    );
    let body = envelope(
        StsOperation::AssumeRole,
        &format!("{nested}{}", standard_credentials()),
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn same_named_elements_in_another_namespace_neither_bind_nor_conflict() {
    let foreign_container = r#"<x:Credentials xmlns:x="urn:example:other"><AccessKeyId>foreign</AccessKeyId></x:Credentials>"#;
    let foreign_member = r#"<x:SessionToken xmlns:x="urn:example:other">foreign</x:SessionToken>"#;
    let credentials = format!(
        "<Credentials>{foreign_member}{}</Credentials>",
        members([ACCESS, SECRET, TOKEN, EXPIRY_TEXT])
    );
    let body = envelope(
        StsOperation::AssumeRole,
        &format!("{foreign_container}{credentials}"),
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn an_element_resolves_with_its_own_declarations_and_undoes_them_on_close() {
    // The first `Credentials` rebinds `sts` onto itself, so it does not bind; the sibling
    // must still see the root binding, which proves the scope was undone.
    let wrong = format!(
        r#"<sts:Credentials xmlns:sts="urn:example:wrong">{}</sts:Credentials>"#,
        members([ACCESS, SECRET, TOKEN, EXPIRY_TEXT])
    );
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}" xmlns:sts="{NAMESPACE}"><AssumeRoleResult>{wrong}{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn nesting_far_past_any_fixed_ceiling_still_decodes() {
    // 70,000 levels with a distinct binding on each: past the `u16` ceiling the rejected
    // `NsReader` design carries, and each close must undo exactly its own scope.
    let depth = 70_000;
    let mut body = format!(r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult>"#);
    for index in 0..depth {
        let _ = write!(body, r#"<w xmlns:w{index}="urn:example:{index}">"#);
    }
    body.push_str(&"</w>".repeat(depth));
    body.push_str(&standard_credentials());
    body.push_str("</AssumeRoleResult></AssumeRoleResponse>");
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn deeply_inherited_names_still_resolve() {
    // 40,000 levels with no new declarations: every name resolves through the inherited root
    // binding in one lookup each.
    let depth = 40_000;
    let mut body = format!(r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult>"#);
    body.push_str(&"<w>".repeat(depth));
    body.push_str(&"</w>".repeat(depth));
    body.push_str(&standard_credentials());
    body.push_str("</AssumeRoleResult></AssumeRoleResponse>");
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn wide_tags_with_thousands_of_attributes_still_decode() {
    let mut attributes = String::new();
    for index in 0..2000 {
        let _ = write!(attributes, r#" a{index}="v{index}""#);
    }
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide{attributes}/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn declarations_apply_to_the_whole_tag_independent_of_order() {
    // The prefixed attribute precedes its own declaration: resolving attributes in tag order
    // would fail this as an unknown prefix.
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><p:Wide p:key="1" xmlns:p="urn:example:other"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
    // The same permutation on a real member: the element name and a prefixed attribute both
    // precede the declarations they resolve under.
    let credentials = format!(
        r#"<Credentials><s:AccessKeyId t:k="1" xmlns:s="{NAMESPACE}" xmlns:t="{NAMESPACE}">{ACCESS}</s:AccessKeyId><SecretAccessKey>{SECRET}</SecretAccessKey><SessionToken>{TOKEN}</SessionToken><Expiration>{EXPIRY_TEXT}</Expiration></Credentials>"#
    );
    assert_standard(&accepted(
        &envelope(StsOperation::AssumeRole, &credentials),
        StsOperation::AssumeRole,
    ));
    // Declaration-first order keeps working too.
    let declared_first = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><p:Wide xmlns:p="urn:example:other" p:key="1"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&declared_first, StsOperation::AssumeRole));
}

#[test]
fn one_huge_uri_with_many_prefixed_attributes_stays_linear() {
    // One 32 KiB namespace URI paired with hundreds of same-namespace attributes: the URI is
    // interned once at declaration time, so per-attribute work never touches its bytes again.
    let uri = format!("urn:example:{}", "x".repeat(32 * 1024));
    let mut attributes = String::new();
    for index in 0..500 {
        let _ = write!(attributes, r#" p:a{index}="{index}""#);
    }
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns:p="{uri}"{attributes}/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn thousands_of_namespace_bindings_on_one_element_still_decode() {
    let mut declarations = String::new();
    for index in 0..2000 {
        let _ = write!(declarations, r#" xmlns:x{index}="urn:example:{index}""#);
    }
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide{declarations}/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn text_and_cdata_split_by_markup_are_joined_in_order() {
    for field in STRING_FIELDS {
        for (inner, expected) in SPLIT_CASES {
            let body = with_field(StsOperation::AssumeRole, field, inner);
            let credential = accepted(&body, StsOperation::AssumeRole);
            assert_eq!(
                field_value(&credential, field),
                *expected,
                "{field}: {inner}"
            );
        }
    }
}

fn collect_scalar(body: &str) -> Cow<'_, str> {
    let mut scanner = Scanner::new(body);
    let mut accum = Accum::Empty;
    loop {
        match scanner.next().expect("the probe is well-formed") {
            Token::Element { .. } | Token::End | Token::Comment | Token::Instruction => {}
            Token::Text(fragment) | Token::CData(fragment) => accum.push_fragment(fragment),
            Token::Char(character) => accum.push_char(character),
            Token::Eof => break,
        }
    }
    accum.finish()
}

#[test]
fn a_single_fragment_is_borrowed_from_the_input_and_a_split_value_is_joined() {
    let body = "<a><!--lead-->plain<!--tail--></a>";
    let Cow::Borrowed(fragment) = collect_scalar(body) else {
        panic!("a single text fragment must not be copied");
    };
    assert_eq!(fragment, "plain");
    assert!(body.as_bytes().as_ptr_range().contains(&fragment.as_ptr()));

    let joined = collect_scalar("<a>pl<!--c-->ain</a>");
    assert!(matches!(&joined, Cow::Owned(text) if text == "plain"));
}

#[test]
fn whitespace_is_kept_in_the_value_and_then_refused_never_dropped() {
    for field in STRING_FIELDS {
        for inner in WHITESPACE_CASES {
            let body = with_field(StsOperation::AssumeRole, field, inner);
            assert_eq!(
                refused(&body, StsOperation::AssumeRole),
                StsDecodeError::InvalidScalar {
                    element: field,
                    problem: ScalarProblem::WhitespaceOrControl
                },
                "{field}: {inner:?}"
            );
        }
    }
}

#[test]
fn a_pretty_printed_session_token_is_refused_not_stripped() {
    assert_eq!(
        refused(WRAPPED_TOKEN_SAMPLE, StsOperation::AssumeRole),
        StsDecodeError::InvalidScalar {
            element: "SessionToken",
            problem: ScalarProblem::WhitespaceOrControl
        }
    );
}

#[test]
fn empty_scalars_are_refused_once_comments_and_instructions_are_skipped() {
    for field in FIELDS {
        for inner in EMPTY_CASES {
            let body = with_field(StsOperation::AssumeRole, field, inner);
            assert_eq!(
                refused(&body, StsOperation::AssumeRole),
                StsDecodeError::InvalidScalar {
                    element: field,
                    problem: ScalarProblem::Empty
                },
                "{field}: {inner}"
            );
        }
    }
    let self_closing = envelope(
        StsOperation::AssumeRole,
        &format!(
            "<Credentials><AccessKeyId>{ACCESS}</AccessKeyId><SecretAccessKey>{SECRET}</SecretAccessKey><SessionToken/><Expiration>{EXPIRY_TEXT}</Expiration></Credentials>"
        ),
    );
    assert_eq!(
        refused(&self_closing, StsOperation::AssumeRole),
        StsDecodeError::InvalidScalar {
            element: "SessionToken",
            problem: ScalarProblem::Empty
        }
    );
}

#[test]
fn expiry_accepts_offsets_fractions_and_split_text() {
    let forms = [
        "2026-01-15T12:00:00Z",
        "2026-01-15T12:00:00.987Z",
        "2026-01-15T13:00:00+01:00",
        "2026-01-15T07:00:00-05:00",
        "2026-01-15T12:<!--split-->00:00Z",
        "<![CDATA[2026-01-15T12:00:00Z]]>",
    ];
    for inner in forms {
        let body = with_field(StsOperation::AssumeRole, "Expiration", inner);
        assert_eq!(
            accepted(&body, StsOperation::AssumeRole).expires_at,
            Some(EXPIRES_AT),
            "{inner}"
        );
    }
}

#[test]
fn an_expiry_that_is_not_a_timestamp_with_an_offset_is_refused() {
    let forms = [
        "tomorrow",
        "2026-01-15",
        "2026-01-15T12:00:00",
        " 2026-01-15T12:00:00Z",
        "2026-01-15T12:00:00Z ",
        "2026-01-15T12:00:00Z\n",
        "2026-13-45T25:61:61Z",
        "1768478400",
        "2026-01-15T12:00:00Z+extra",
    ];
    for inner in forms {
        let body = with_field(StsOperation::AssumeRole, "Expiration", inner);
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::InvalidScalar {
                element: "Expiration",
                problem: ScalarProblem::NotTimestamp
            },
            "{inner:?}"
        );
    }
}

#[test]
fn a_body_decodes_only_under_the_operation_it_answers() {
    for operation in BOTH {
        assert_eq!(
            refused(&standard(operation), other(operation)),
            StsDecodeError::UnexpectedRoot {
                expected: names(other(operation)).0
            }
        );
    }
}

#[test]
fn a_root_outside_the_sts_namespace_is_refused() {
    let unqualified = |attribute: &str| {
        format!(
            "<AssumeRoleResponse{attribute}><AssumeRoleResult>{}</AssumeRoleResult></AssumeRoleResponse>",
            standard_credentials()
        )
    };
    let prefixed_elsewhere = format!(
        r#"<s:AssumeRoleResponse xmlns:s="urn:example:other"><s:AssumeRoleResult>{}</s:AssumeRoleResult></s:AssumeRoleResponse>"#,
        standard_credentials()
    );
    let bodies = [
        unqualified(""),
        unqualified(r#" xmlns="https://sts.amazonaws.com/doc/2011-06-15""#),
        unqualified(r#" xmlns="http://sts.amazonaws.com/doc/2011-06-15/""#),
        unqualified(r#" xmlns="urn:example:other""#),
        prefixed_elsewhere,
    ];
    for body in &bodies {
        assert_eq!(
            refused(body, StsOperation::AssumeRole),
            StsDecodeError::UnexpectedRoot {
                expected: "AssumeRoleResponse"
            }
        );
    }
}

#[test]
fn a_result_or_member_outside_the_sts_namespace_is_not_the_required_element() {
    let foreign_result = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult xmlns="urn:example:other">{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_eq!(
        refused(&foreign_result, StsOperation::AssumeRole),
        StsDecodeError::Missing {
            element: "AssumeRoleResult"
        }
    );
    for declaration in [r#"xmlns="urn:example:other""#, r#"xmlns="""#] {
        let credentials = format!(
            "<Credentials><AccessKeyId {declaration}>{ACCESS}</AccessKeyId><SecretAccessKey>{SECRET}</SecretAccessKey><SessionToken>{TOKEN}</SessionToken><Expiration>{EXPIRY_TEXT}</Expiration></Credentials>"
        );
        assert_eq!(
            refused(
                &envelope(StsOperation::AssumeRole, &credentials),
                StsOperation::AssumeRole
            ),
            StsDecodeError::Missing {
                element: "AccessKeyId"
            },
            "{declaration}"
        );
    }
}

#[test]
fn the_result_of_the_other_operation_does_not_satisfy_the_requested_one() {
    for operation in BOTH {
        let (root, _) = names(operation);
        let (_, foreign_result) = names(other(operation));
        let body = format!(
            r#"<{root} xmlns="{NAMESPACE}"><{foreign_result}>{}</{foreign_result}></{root}>"#,
            standard_credentials()
        );
        assert_eq!(
            refused(&body, operation),
            StsDecodeError::Missing {
                element: names(operation).1
            }
        );
    }
}

#[test]
fn credentials_are_bound_to_the_result_not_found_by_depth_or_position() {
    let beside_the_result = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult/>{}</AssumeRoleResponse>"#,
        standard_credentials()
    );
    let too_deep = envelope(
        StsOperation::AssumeRole,
        &format!(
            "<AssumedRoleUser>{}</AssumedRoleUser>",
            standard_credentials()
        ),
    );
    for body in [beside_the_result, too_deep] {
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::Missing {
                element: "Credentials"
            }
        );
    }
}

#[test]
fn a_repeated_container_is_refused_not_resolved_by_first_or_last() {
    let second = credentials_from([
        "ASIAPLANTEDSECOND",
        "second-secret",
        "second-token",
        EXPIRY_TEXT,
    ]);
    let two_results = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult>{}</AssumeRoleResult><AssumeRoleResult>{second}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_eq!(
        refused(&two_results, StsOperation::AssumeRole),
        StsDecodeError::Duplicate {
            element: "AssumeRoleResult"
        }
    );
    let two_credentials = envelope(
        StsOperation::AssumeRole,
        &format!("{}{second}", standard_credentials()),
    );
    assert_eq!(
        refused(&two_credentials, StsOperation::AssumeRole),
        StsDecodeError::Duplicate {
            element: "Credentials"
        }
    );
}

#[test]
fn a_repeated_scalar_member_is_refused_even_when_the_values_agree() {
    for field in FIELDS {
        for repeated in ["second-value", "equal-value"] {
            let standard_members = members([ACCESS, SECRET, TOKEN, EXPIRY_TEXT]);
            let credentials = format!(
                "<Credentials>{standard_members}<{field}>{repeated}</{field}></Credentials>"
            );
            assert_eq!(
                refused(
                    &envelope(StsOperation::AssumeRole, &credentials),
                    StsOperation::AssumeRole
                ),
                StsDecodeError::Duplicate { element: field },
                "{field}"
            );
        }
    }
}

#[test]
fn markup_inside_a_scalar_is_refused() {
    for field in FIELDS {
        for inner in NESTED_CASES {
            let body = with_field(StsOperation::AssumeRole, field, inner);
            assert_eq!(
                refused(&body, StsOperation::AssumeRole),
                StsDecodeError::NestedMarkup { element: field },
                "{field}: {inner}"
            );
        }
    }
}

#[test]
fn every_missing_element_is_named() {
    for operation in BOTH {
        for field in FIELDS {
            let body = envelope(operation, &credentials_without(field));
            assert_eq!(
                refused(&body, operation),
                StsDecodeError::Missing { element: field }
            );
        }
        assert_eq!(
            refused(&envelope(operation, "<AssumedRoleUser/>"), operation),
            StsDecodeError::Missing {
                element: "Credentials"
            }
        );
        let (root, result) = names(operation);
        let no_result = format!(r#"<{root} xmlns="{NAMESPACE}"><ResponseMetadata/></{root}>"#);
        assert_eq!(
            refused(&no_result, operation),
            StsDecodeError::Missing { element: result }
        );
    }
}

#[test]
fn content_after_a_complete_document_is_refused() {
    let valid = standard(StsOperation::AssumeRole);
    let bodies = [
        format!("{valid}<Extra/>"),
        format!("{valid}trailing text"),
        format!("{valid}</AssumeRoleResponse>"),
        format!("{valid}<!-- unterminated"),
        format!("{valid}<?pi"),
        format!("{valid}{valid}"),
        format!("junk{valid}"),
        String::new(),
        String::from("   "),
        String::from("<"),
    ];
    for body in &bodies {
        assert_eq!(
            refused(body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            }
        );
    }
}

#[test]
fn double_hyphen_comments_are_refused() {
    let body = with_field(
        StsOperation::AssumeRole,
        "AccessKeyId",
        "AB<!--bad--comment-->CD",
    );
    assert_eq!(
        refused(&body, StsOperation::AssumeRole),
        StsDecodeError::Xml {
            fault: XmlFault::Malformed
        }
    );
}

#[test]
fn trailing_cdata_and_references_are_refused() {
    let valid = standard(StsOperation::AssumeRole);
    for body in [format!("{valid}<![CDATA[x]]>"), format!("{valid}&#65;")] {
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            }
        );
    }
}

#[test]
fn a_declaration_bom_and_surrounding_comments_are_valid_xml_and_change_nothing() {
    let valid = standard(StsOperation::AssumeRole);
    let bodies = [
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{valid}"),
        format!("{valid}\n<!-- audit -->\n<?audit done?>\n"),
        format!("  {valid}"),
        format!("<!-- lead -->{valid}"),
    ];
    for body in &bodies {
        assert_standard(&accepted(body, StsOperation::AssumeRole));
    }
}

#[test]
fn the_declaration_must_lead_with_a_known_version_and_ordered_names() {
    let valid = standard(StsOperation::AssumeRole);
    let bad_decls = [
        r#"<?xml encoding="utf-8" version="1.0"?>"#,
        r#"<?xml version="1.2"?>"#,
        r"<?xml?>",
        r#"<?xml version="1.0" bogus="x"?>"#,
        r#"<?xml version="1.0" standalone="maybe"?>"#,
        r#"<?xml version="1.0" standalone="yes" encoding="utf-8"?>"#,
        r#"<?xml version="1.0" encoding=""?>"#,
        r#"<?xml version="1.0" version="1.1"?>"#,
    ];
    for decl in bad_decls {
        assert_eq!(
            refused(&format!("{decl}{valid}"), StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            },
            "{decl}"
        );
    }
    for decl in [
        r#"<?xml version="1.0"?>"#,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<?xml version="1.1"?>"#,
    ] {
        assert_standard(&accepted(
            &format!("{decl}{valid}"),
            StsOperation::AssumeRole,
        ));
    }
    for body in [
        format!(r#"<?xml version="1.0"?><?xml version="1.0"?>{valid}"#),
        format!(r#"<!-- c --><?xml version="1.0"?>{valid}"#),
        format!(r#"{valid}<?xml version="1.0"?>"#),
    ] {
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            }
        );
    }
}

#[test]
fn element_and_attribute_names_and_pi_targets_are_validated() {
    let bodies = [
        format!(
            r#"<1bad xmlns="{NAMESPACE}"><AssumeRoleResult>{}</AssumeRoleResult></1bad>"#,
            standard_credentials()
        ),
        format!(
            r#"<:AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult>{}</AssumeRoleResult></:AssumeRoleResponse>"#,
            standard_credentials()
        ),
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Credentials foo>{}</Credentials></AssumeRoleResult></AssumeRoleResponse>"#,
            members([ACCESS, SECRET, TOKEN, EXPIRY_TEXT])
        ),
        format!(
            r#"<?xml-stylesheet href="x"?>{}"#,
            standard(StsOperation::AssumeRole)
        ),
        format!(r"<?XML foo?>{}", standard(StsOperation::AssumeRole)),
    ];
    for body in &bodies {
        assert_eq!(
            refused(body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            }
        );
    }
}

#[test]
fn invalid_characters_and_character_references_are_refused() {
    let malformed = [
        with_field(StsOperation::AssumeRole, "AccessKeyId", "a\u{1}b"),
        envelope(
            StsOperation::AssumeRole,
            &format!(
                "<FutureMember bad=\"x\u{1}y\">x</FutureMember>{}",
                standard_credentials()
            ),
        ),
        with_field(
            StsOperation::AssumeRole,
            "SecretAccessKey",
            "<![CDATA[a\u{1}b]]>",
        ),
        with_field(StsOperation::AssumeRole, "SessionToken", "&#1;"),
        with_field(StsOperation::AssumeRole, "Expiration", "&#xb;"),
    ];
    for body in &malformed {
        assert_eq!(
            refused(body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            }
        );
    }
    for body in [
        with_field(StsOperation::AssumeRole, "AccessKeyId", "&#xD800;"),
        with_field(StsOperation::AssumeRole, "AccessKeyId", "&#x110000;"),
        with_field(StsOperation::AssumeRole, "AccessKeyId", "&#;"),
    ] {
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::EntityReference
            }
        );
    }
}

#[test]
fn attribute_names_bind_by_expanded_name_and_reserved_rules() {
    let malformed_bodies = [
        // Two spellings, one expanded attribute name.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns:a="urn:dup" xmlns:b="urn:dup" a:id="1" b:id="2"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // Byte-identical duplicate attribute.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide id="1" id="2"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // `xmlns` rebound onto a prefix.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns:xmlns="urn:other"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // `xml` rebound away from its namespace.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns:xml="urn:other"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // A plain prefix bound to the `xml` namespace.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns:p="http://www.w3.org/XML/1998/namespace"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // The default namespace bound to the `xml` namespace.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns="http://www.w3.org/XML/1998/namespace"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // The `xmlns` prefix used as an element prefix.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><xmlns:Wide/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // A prefix with no binding on the element.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><p:Wide/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
        // A prefix with no binding on an attribute.
        format!(
            r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide p:key="1"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
            standard_credentials()
        ),
    ];
    for body in &malformed_bodies {
        assert_eq!(
            refused(body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Malformed
            }
        );
    }
    // One local name in two namespaces is two different attributes.
    let body = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Wide xmlns:a="urn:u1" xmlns:b="urn:u2" a:k="1" b:k="2"/>{}</AssumeRoleResult></AssumeRoleResponse>"#,
        standard_credentials()
    );
    assert_standard(&accepted(&body, StsOperation::AssumeRole));
}

#[test]
fn no_proper_prefix_of_a_valid_body_yields_credentials() {
    for operation in BOTH {
        let body = standard(operation);
        assert!(body.is_ascii(), "the fixture is sliced at every byte");
        for end in 0..body.len() {
            let error = refused(&body[..end], operation);
            assert!(
                matches!(error, StsDecodeError::Xml { .. }),
                "prefix of {end} bytes"
            );
        }
    }
}

#[test]
fn entity_declarations_are_refused_before_any_expansion() {
    let internal = with_prolog(
        r#"<?xml version="1.0"?><!DOCTYPE AssumeRoleResponse [<!ENTITY planted "PLANTED_ENTITY_VALUE">]>"#,
        "&planted;",
    );
    let external = with_prolog(
        r#"<!DOCTYPE AssumeRoleResponse [<!ENTITY secret SYSTEM "file:///etc/passwd">]>"#,
        "&secret;",
    );
    let laughs = with_prolog(
        r#"<!DOCTYPE AssumeRoleResponse [<!ENTITY a "lol"><!ENTITY b "&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;">]>"#,
        "&c;",
    );
    let external_dtd = with_prolog(
        r#"<!DOCTYPE AssumeRoleResponse SYSTEM "http://127.0.0.1:9/evil.dtd">"#,
        "plain",
    );
    for body in [internal, external, laughs, external_dtd] {
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::Dtd
            }
        );
    }
}

#[test]
fn an_empty_doctype_is_refused_outright() {
    let body = with_prolog("<!DOCTYPE AssumeRoleResponse>", "plain");
    assert_eq!(
        refused(&body, StsOperation::AssumeRole),
        StsDecodeError::Xml {
            fault: XmlFault::Dtd
        }
    );
}

#[test]
fn an_empty_doctype_never_makes_an_entity_reference_resolve() {
    // Whether a DOCTYPE that declares nothing is itself accepted is the backend's rule and is
    // pinned by the test above. What must hold either way: no credential comes out of a body
    // that references an entity under such a DOCTYPE.
    let body = with_prolog("<!DOCTYPE AssumeRoleResponse>", "&nothing;");
    let error = refused(&body, StsOperation::AssumeRole);
    assert!(
        matches!(
            error,
            StsDecodeError::Xml {
                fault: XmlFault::Dtd | XmlFault::EntityReference
            }
        ),
        "{error}"
    );
}

#[test]
fn undeclared_and_malformed_entity_references_fail_the_parse() {
    for inner in ["&undeclared;", "AB&CD", "&amp", "&#xZZ;"] {
        let body = with_field(StsOperation::AssumeRole, "AccessKeyId", inner);
        assert_eq!(
            refused(&body, StsOperation::AssumeRole),
            StsDecodeError::Xml {
                fault: XmlFault::EntityReference
            },
            "{inner}"
        );
    }
}

struct Probe {
    label: &'static str,
    body: String,
    library_echo: Option<&'static str>,
}

fn probes() -> Vec<Probe> {
    let operation = StsOperation::AssumeRole;
    let duplicate_secret = envelope(
        operation,
        &format!(
            "{}<SecretAccessKey>PLANTED_SECRET</SecretAccessKey></Credentials>",
            standard_credentials().trim_end_matches("</Credentials>")
        ),
    );
    let mismatched_close_tag = format!(
        r#"<AssumeRoleResponse xmlns="{NAMESPACE}"><AssumeRoleResult><Credentials><SecretAccessKey>PLANTED_SECRET</PLANTED_TAG></Credentials></AssumeRoleResult></AssumeRoleResponse>"#
    );
    let unknown_prefix = envelope(
        operation,
        &format!(
            "<PLANTED_PREFIX:Thing>PLANTED_SECRET</PLANTED_PREFIX:Thing>{}",
            standard_credentials()
        ),
    );
    let duplicate_attribute = envelope(
        operation,
        &format!(
            r#"<Credentials><AccessKeyId PLANTED_ATTR="PLANTED_SECRET" PLANTED_ATTR="PLANTED_SECRET">{ACCESS}</AccessKeyId><SecretAccessKey>{SECRET}</SecretAccessKey><SessionToken>{TOKEN}</SessionToken><Expiration>{EXPIRY_TEXT}</Expiration></Credentials>"#
        ),
    );
    vec![
        Probe {
            label: "duplicate secret",
            body: duplicate_secret,
            library_echo: None,
        },
        Probe {
            label: "nested markup",
            body: with_field(
                operation,
                "SessionToken",
                "PLANTED_TOKEN<b>PLANTED_TOKEN</b>",
            ),
            library_echo: None,
        },
        Probe {
            label: "bad expiry",
            body: with_field(operation, "Expiration", "PLANTED_EXPIRY"),
            library_echo: None,
        },
        Probe {
            label: "whitespace secret",
            body: with_field(
                operation,
                "SecretAccessKey",
                "PLANTED_SECRET <!--c--> PLANTED_SECRET",
            ),
            library_echo: None,
        },
        Probe {
            label: "mismatched close tag",
            body: mismatched_close_tag,
            library_echo: Some("PLANTED_TAG"),
        },
        Probe {
            label: "unknown prefix",
            body: unknown_prefix,
            library_echo: None,
        },
        Probe {
            label: "unknown entity",
            body: with_field(operation, "AccessKeyId", "&PLANTED_ENTITY;"),
            library_echo: None,
        },
        Probe {
            label: "doctype entity",
            body: with_prolog(
                r#"<!DOCTYPE AssumeRoleResponse [<!ENTITY e "PLANTED_SECRET">]>"#,
                "&e;",
            ),
            library_echo: None,
        },
        Probe {
            label: "foreign root",
            body: format!(r#"<PLANTED_TAG xmlns="{NAMESPACE}">PLANTED_SECRET</PLANTED_TAG>"#),
            library_echo: None,
        },
        Probe {
            label: "duplicate attribute",
            body: duplicate_attribute,
            library_echo: None,
        },
        Probe {
            label: "unnamed decoy",
            body: envelope(operation, "<PLANTED_TAG>PLANTED_SECRET</PLANTED_TAG>"),
            library_echo: None,
        },
    ]
}

#[test]
fn refusals_never_render_response_text_or_credential_values() {
    for probe in probes() {
        let error = refused(&probe.body, StsOperation::AssumeRole);
        assert_redacted(&error, probe.label);
    }
}

#[test]
fn the_mapping_discards_library_text_that_would_have_echoed_the_input() {
    let echoing: Vec<(&str, &str, String)> = probes()
        .into_iter()
        .filter_map(|probe| {
            probe
                .library_echo
                .map(|echo| (probe.label, echo, probe.body))
        })
        .collect();
    assert_eq!(echoing.len(), 1);
    for (label, echo, body) in echoing {
        let mut reader = Reader::from_str(&body);
        let mut library = None;
        loop {
            match reader.read_event() {
                Err(error) => {
                    library = Some(error);
                    break;
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
            }
        }
        let library = library.expect("the probe fails at the library level");
        assert!(library.to_string().contains(echo), "{label}");
    }
}

#[test]
fn error_texts_name_only_static_elements() {
    assert_eq!(
        StsDecodeError::Missing {
            element: "SessionToken"
        }
        .to_string(),
        "the STS response has no SessionToken element"
    );
    assert_eq!(
        StsDecodeError::Duplicate {
            element: "Credentials"
        }
        .to_string(),
        "the STS response has more than one Credentials element"
    );
    assert_eq!(
        StsDecodeError::NestedMarkup {
            element: "Expiration"
        }
        .to_string(),
        "the STS Expiration element contains nested markup"
    );
    assert_eq!(
        StsDecodeError::UnexpectedRoot {
            expected: "AssumeRoleResponse"
        }
        .to_string(),
        "the STS response root is not an AssumeRoleResponse element in the STS namespace"
    );
    assert_eq!(
        StsDecodeError::InvalidScalar {
            element: "SecretAccessKey",
            problem: ScalarProblem::WhitespaceOrControl
        }
        .to_string(),
        "the STS SecretAccessKey element is not usable as a credential value because it contains whitespace or a control character"
    );
    assert_eq!(
        StsDecodeError::Xml {
            fault: XmlFault::Dtd
        }
        .to_string(),
        "the STS response body is not acceptable XML (DTD or entity declaration)"
    );
}

const DOCUMENTED_CODES: &[(&str, StsErrorCode)] = &[
    ("AccessDenied", StsErrorCode::AccessDenied),
    ("AccessDeniedException", StsErrorCode::AccessDeniedException),
    ("ExpiredTokenException", StsErrorCode::ExpiredTokenException),
    ("IncompleteSignature", StsErrorCode::IncompleteSignature),
    ("InternalFailure", StsErrorCode::InternalFailure),
    (
        "InvalidParameterCombination",
        StsErrorCode::InvalidParameterCombination,
    ),
    ("InvalidParameterValue", StsErrorCode::InvalidParameterValue),
    ("InvalidQueryParameter", StsErrorCode::InvalidQueryParameter),
    ("MalformedQueryString", StsErrorCode::MalformedQueryString),
    ("MissingAction", StsErrorCode::MissingAction),
    (
        "MissingAuthenticationToken",
        StsErrorCode::MissingAuthenticationToken,
    ),
    ("MissingParameter", StsErrorCode::MissingParameter),
    ("NotAuthorized", StsErrorCode::NotAuthorized),
    ("OptInRequired", StsErrorCode::OptInRequired),
    ("RequestExpired", StsErrorCode::RequestExpired),
    ("ServiceUnavailable", StsErrorCode::ServiceUnavailable),
    ("ThrottlingException", StsErrorCode::ThrottlingException),
    (
        "UnrecognizedClientException",
        StsErrorCode::UnrecognizedClientException,
    ),
    ("ValidationError", StsErrorCode::ValidationError),
    ("ExpiredToken", StsErrorCode::ExpiredToken),
    ("IDPCommunicationError", StsErrorCode::IDPCommunicationError),
    ("IDPRejectedClaim", StsErrorCode::IDPRejectedClaim),
    ("InvalidIdentityToken", StsErrorCode::InvalidIdentityToken),
    (
        "MalformedPolicyDocument",
        StsErrorCode::MalformedPolicyDocument,
    ),
    ("PackedPolicyTooLarge", StsErrorCode::PackedPolicyTooLarge),
    ("RegionDisabled", StsErrorCode::RegionDisabled),
    (
        "RegionDisabledException",
        StsErrorCode::RegionDisabledException,
    ),
];

fn error_response(code_inner: &str) -> String {
    format!(
        r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Type>Sender</Type><Code>{code_inner}</Code><Message>PLANTED_MESSAGE</Message></Error><RequestId>7b4a26e8-2cb6-49a0-9764-58a43f88f123</RequestId></ErrorResponse>"#
    )
}

#[test]
fn every_documented_error_code_maps_to_its_own_variant() {
    for (wire, expected) in DOCUMENTED_CODES {
        assert_eq!(
            decode_error_code(&error_response(wire)),
            Ok(*expected),
            "{wire}"
        );
    }
}

#[test]
fn only_the_code_is_read_from_an_error_body() {
    let body = format!(
        r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Type>Sender</Type><Type>Receiver</Type><Code>ExpiredToken</Code><Message>PLANTED_MESSAGE<b/><!--c--></Message><Message>again</Message></Error><RequestId>r1</RequestId><RequestId>r2</RequestId><Extra/></ErrorResponse>"#
    );
    assert_eq!(decode_error_code(&body), Ok(StsErrorCode::ExpiredToken));
}

#[test]
fn error_code_text_split_by_markup_is_joined() {
    assert_eq!(
        decode_error_code(&error_response("Expired<!--split-->Token")),
        Ok(StsErrorCode::ExpiredToken)
    );
    assert_eq!(
        decode_error_code(&error_response("<![CDATA[Expired]]><?pi?>TokenException")),
        Ok(StsErrorCode::ExpiredTokenException)
    );
}

#[test]
fn unknown_error_codes_are_unrecognized_and_keep_nothing_of_the_text() {
    let spellings = [
        "PLANTED_CODE",
        "expiredtoken",
        " ExpiredToken",
        "ExpiredToken ",
        "ExpiredToken\n",
        "ExpiredToken2",
        "Expired Token",
    ];
    for spelling in spellings {
        let code = decode_error_code(&error_response(spelling)).expect("the envelope decodes");
        assert_eq!(code, StsErrorCode::Unrecognized, "{spelling:?}");
        assert_eq!(format!("{code:?}"), "Unrecognized");
    }
}

#[test]
fn the_error_envelope_structure_is_enforced() {
    let cases = [
        (
            r"<ErrorResponse><Error><Code>ExpiredToken</Code></Error></ErrorResponse>".to_owned(),
            StsDecodeError::UnexpectedRoot { expected: "ErrorResponse" },
        ),
        (
            r#"<ErrorResponse xmlns="urn:example:other"><Error><Code>ExpiredToken</Code></Error></ErrorResponse>"#.to_owned(),
            StsDecodeError::UnexpectedRoot { expected: "ErrorResponse" },
        ),
        (
            format!(r#"<ErrorResponse xmlns="{NAMESPACE}"/>"#),
            StsDecodeError::Missing { element: "Error" },
        ),
        (
            format!(r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Type>Sender</Type></Error></ErrorResponse>"#),
            StsDecodeError::Missing { element: "Code" },
        ),
        (
            format!(
                r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Code>ExpiredToken</Code></Error><Error><Code>InternalFailure</Code></Error></ErrorResponse>"#
            ),
            StsDecodeError::Duplicate { element: "Error" },
        ),
        (
            format!(
                r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Code>ExpiredToken</Code><Code>InternalFailure</Code></Error></ErrorResponse>"#
            ),
            StsDecodeError::Duplicate { element: "Code" },
        ),
        (error_response(""), StsDecodeError::InvalidScalar { element: "Code", problem: ScalarProblem::Empty }),
        (error_response("Expired<b>x</b>Token"), StsDecodeError::NestedMarkup { element: "Code" }),
        (
            format!(
                r#"<ErrorResponse xmlns="{NAMESPACE}"><Error xmlns="urn:example:other"><Code>ExpiredToken</Code></Error></ErrorResponse>"#
            ),
            StsDecodeError::Missing { element: "Code" },
        ),
        (
            format!(
                r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Code xmlns="urn:example:other">ExpiredToken</Code></Error></ErrorResponse>"#
            ),
            StsDecodeError::Missing { element: "Code" },
        ),
        (
            format!(
                r#"<ErrorResponse xmlns="{NAMESPACE}"><Wrapper><Error><Code>ExpiredToken</Code></Error></Wrapper></ErrorResponse>"#
            ),
            StsDecodeError::Missing { element: "Code" },
        ),
        (
            error_response("ExpiredToken").replace("</ErrorResponse>", ""),
            StsDecodeError::Xml { fault: XmlFault::Malformed },
        ),
        (
            format!("{}<Extra/>", error_response("ExpiredToken")),
            StsDecodeError::Xml { fault: XmlFault::Malformed },
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(decode_error_code(&body), Err(expected));
    }
}

#[test]
fn success_and_error_documents_are_not_interchangeable() {
    for operation in BOTH {
        assert_eq!(
            decode_error_code(&standard(operation)),
            Err(StsDecodeError::UnexpectedRoot {
                expected: "ErrorResponse"
            })
        );
        assert_eq!(
            refused(&error_response("ExpiredToken"), operation),
            StsDecodeError::UnexpectedRoot {
                expected: names(operation).0
            }
        );
    }
}

#[test]
fn error_code_failures_never_render_response_text() {
    let bodies = [
        r#"<ErrorResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><PLANTED_TAG>PLANTED_MESSAGE</PLANTED_TAG></ErrorResponse>"#.to_owned(),
        error_response("&PLANTED_ENTITY;"),
        error_response("PLANTED_CODE<b>PLANTED_CODE</b>"),
        format!(
            r#"<ErrorResponse xmlns="{NAMESPACE}"><Error><Code>PLANTED_CODE</Code><Code>PLANTED_CODE</Code></Error></ErrorResponse>"#
        ),
        r#"<PLANTED_TAG xmlns="https://sts.amazonaws.com/doc/2011-06-15/">PLANTED_MESSAGE</PLANTED_TAG>"#.to_owned(),
        r"<ErrorResponse><PLANTED_PREFIX:Thing>PLANTED_MESSAGE</PLANTED_PREFIX:Thing></ErrorResponse>".to_owned(),
    ];
    for body in &bodies {
        let error = decode_error_code(body).expect_err("the body is refused");
        assert_redacted(&error, "error body");
    }
}
