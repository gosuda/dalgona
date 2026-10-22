use super::*;

/// The well-known AWS `SigV4` test credential from the AWS documentation
/// example (GET vanilla object, service `s3` shape adapted to `bedrock`).
fn credential() -> AwsCredential {
    AwsCredential {
        access_key_id: "AKIDEXAMPLE".into(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        session_token: None,
        expires_at: None,
    }
}

fn date(long: &str) -> AmzDate {
    AmzDate {
        long: long.to_owned(),
        short: long[..8].to_owned(),
    }
}

fn signed_auth(request: &SignedRequest<'_>, body: &[u8]) -> String {
    request.sign(body).authorization
}

#[test]
fn derive_signing_key_matches_the_aws_documentation_vector() {
    // https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html
    let key = signing_key(
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        "20150830",
        "us-east-1",
        "iam",
    );
    assert_eq!(
        hex(&key),
        "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
    );
}

#[test]
fn canonical_path_encodes_each_segment_and_keeps_slashes() {
    assert_eq!(
        canonical_path("/model/anthropic.claude-4:0/converse-stream"),
        "/model/anthropic.claude-4%3A0/converse-stream"
    );
    assert_eq!(
        canonical_path("/model/arn%3Aaws%3Abedrock/converse-stream"),
        "/model/arn%253Aaws%253Abedrock/converse-stream"
    );
    assert_eq!(canonical_path("/"), "/");
}

#[test]
fn canonical_query_sorts_by_the_encoded_form() {
    // The wire spelling is decoded first, so an escape stays one escape.
    let query = "b=2&a=%7B1%7D&A=1";
    assert_eq!(canonical_query(Some(query)), "A=1&a=%7B1%7D&b=2");
    // Sorting happens on the encoded form: `%` (0x25) sorts before `x`
    // (0x78), though the decoded `{` (0x7B) sorts after it.
    assert_eq!(canonical_query(Some("x=2&%7B=1")), "%7B=1&x=2");
    // Duplicate names order by encoded value.
    assert_eq!(canonical_query(Some("a=2&a=1")), "a=1&a=2");
    assert_eq!(canonical_query(None), "");
    assert_eq!(canonical_query(Some("")), "");
}

#[test]
fn canonical_query_encodes_the_decoded_bytes_once() {
    // A reserved byte arriving escaped or raw signs the same way.
    assert_eq!(
        canonical_query(Some("a=b%20c")),
        canonical_query(Some("a=b c"))
    );
    assert_eq!(canonical_query(Some("a=b%20c")), "a=b%20c");
    assert_eq!(canonical_query(Some("a=b%2fc")), "a=b%2Fc");
    // `+` is a literal plus, never a space.
    assert_eq!(canonical_query(Some("a=b+c")), "a=b%2Bc");
    // A `%` without two hexadecimal digits is a literal percent sign.
    assert_eq!(canonical_query(Some("a=100%")), "a=100%25");
    assert_eq!(canonical_query(Some("a=%zz&b=%4")), "a=%25zz&b=%254");
    // A decoded byte that is not UTF-8 keeps its single-byte escape.
    assert_eq!(canonical_query(Some("a=%FF")), "a=%FF");
    // A name without `=` carries an empty value.
    assert_eq!(canonical_query(Some("flag&a=1")), "a=1&flag=");
}

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

/// The signature of one request in the shape of the public `SigV4` test-suite
/// cases (credential scope `20150830/us-east-1/service/aws4_request`), built
/// through the same canonical-request, string-to-sign, and signature
/// functions `SignedRequest::sign` composes. `headers` are already sorted.
fn suite_signature(
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> String {
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let (request, _) = canonical_request(method, path, query, &headers, &sha256_hex(body));
    let to_sign = string_to_sign(
        "20150830T123600Z",
        "20150830/us-east-1/service/aws4_request",
        &request,
    );
    signature(SECRET, "20150830", "us-east-1", "service", &to_sign)
}

#[test]
fn sign_matches_known_answers_from_an_independent_signer() {
    // Signatures produced by an independent `SigV4` implementation for the
    // same request shape (`host;x-amz-content-sha256;x-amz-date`), with the
    // test-suite credential and date. Unlike the cases below, these drive
    // `SignedRequest::sign` itself, so the header set, ordering, scope and
    // payload hash all take part.
    let credential = credential();
    let get = SignedRequest {
        method: "GET",
        host: "example.amazonaws.com",
        path: "/",
        query: None,
        headers: &[],
        service: "service",
        region: "us-east-1",
        credential: &credential,
        date: date("20150830T123600Z"),
    }
    .sign(b"");
    assert_eq!(
        get.content_sha256,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        get.authorization,
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
         SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
         Signature=726c5c4879a6b4ccbbd3b24edbd6b8826d34f87450fbbf4e85546fc7ba9c1642"
    );
    let post = SignedRequest {
        method: "POST",
        host: "example.amazonaws.com",
        path: "/",
        query: None,
        headers: &[("content-type", String::from("application/json"))],
        service: "service",
        region: "us-east-1",
        credential: &credential,
        date: date("20150830T123600Z"),
    }
    .sign(br#"{"hello":"world"}"#);
    assert_eq!(
        post.content_sha256,
        "93a23971a914e5eacbf0a8d25154cda309c3c1c72fbb9914d47c60f3cb681588"
    );
    assert_eq!(
        post.authorization,
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
         SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, \
         Signature=e9744044f72be2a6e5082cdcebb673e0a1daf890c82cc130d46abd3769ca15e0"
    );
}

#[test]
fn suite_shaped_requests_match_an_independent_computation() {
    // The constants were computed with an independent HMAC-SHA-256
    // implementation over the canonical requests of these suite-shaped
    // cases; the case names only say which request shape each one is.
    let plain = [
        ("host", "example.amazonaws.com"),
        ("x-amz-date", "20150830T123600Z"),
    ];
    assert_eq!(
        suite_signature("GET", "/", None, &plain, b""),
        "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31",
        "get-vanilla"
    );
    assert_eq!(
        suite_signature("GET", "/", Some("Param2=value2&Param1=value1"), &plain, b""),
        "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500",
        "get-vanilla-query-order-key-case"
    );
    assert_eq!(
        suite_signature("GET", "/", Some("ሴ=bar"), &plain, b""),
        "2cdec8eed098649ff3a119c94853b13c643bcf08f8b0a1d91e12c9027818dd04",
        "get-vanilla-utf8-query"
    );
    assert_eq!(
        suite_signature("GET", "/ሴ", None, &plain, b""),
        "8318018e0b0f223aa2bbf98705b62bb787dc9c0e678f255a891fd03141be5d85",
        "get-utf8"
    );
    assert_eq!(
        suite_signature("GET", "/example space/", None, &plain, b""),
        "652487583200325589f1fba4c7e578f72c47cb61beeca81406b39ddec1366741",
        "get-space"
    );
    assert_eq!(
        suite_signature("POST", "/", Some("Param1=value1"), &plain, b""),
        "28038455d6de14eafc1f9222cf5aa6f1a96197d7deb8263271d420d138af7f11",
        "post-vanilla-query"
    );
    let form = [
        ("content-type", "application/x-www-form-urlencoded"),
        ("host", "example.amazonaws.com"),
        ("x-amz-date", "20150830T123600Z"),
    ];
    assert_eq!(
        suite_signature("POST", "/", None, &form, b"Param1=value1"),
        "ff11897932ad3f4e8b18135d722051e5ac45fc38421b1da7b9d196a0fe09473a",
        "post-x-www-form-urlencoded"
    );
}

#[test]
fn the_iam_list_users_example_matches_an_independent_computation_at_every_stage() {
    // The request of the IAM signing walkthrough (service `iam`); the
    // canonical-request hash and signature were computed independently.
    let headers = [
        (
            String::from("content-type"),
            String::from("application/x-www-form-urlencoded; charset=utf-8"),
        ),
        (String::from("host"), String::from("iam.amazonaws.com")),
        (String::from("x-amz-date"), String::from("20150830T123600Z")),
    ];
    let (request, signed_names) = canonical_request(
        "GET",
        "/",
        Some("Action=ListUsers&Version=2010-05-08"),
        &headers,
        &sha256_hex(b""),
    );
    assert_eq!(signed_names, "content-type;host;x-amz-date");
    assert_eq!(
        sha256_hex(request.as_bytes()),
        "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59"
    );
    let to_sign = string_to_sign(
        "20150830T123600Z",
        "20150830/us-east-1/iam/aws4_request",
        &request,
    );
    assert_eq!(
        signature(SECRET, "20150830", "us-east-1", "iam", &to_sign),
        "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
    );
}

#[test]
fn sign_produces_the_documented_header_set() {
    let credential = credential();
    let request = SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/anthropic.claude-4-sonnet/converse-stream",
        query: None,
        headers: &[("content-type", String::from("application/json"))],
        service: SERVICE,
        region: "us-east-1",
        credential: &credential,
        date: date("20260115T120000Z"),
    };
    let signed = request.sign(b"{}");
    assert_eq!(signed.host, "bedrock-runtime.us-east-1.amazonaws.com");
    assert_eq!(signed.amz_date, "20260115T120000Z");
    // SHA-256 of `{}`.
    assert_eq!(
        signed.content_sha256,
        "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    );
    assert_eq!(signed.security_token, None);
    let (algorithm, rest) = signed
        .authorization
        .split_once(' ')
        .unwrap_or_else(|| panic!("authorization carries a space"));
    assert_eq!(algorithm, "AWS4-HMAC-SHA256");
    let (credential_pair, rest) = rest
        .split_once(", ")
        .unwrap_or_else(|| panic!("credential pair first"));
    let (signed_headers, signature) = rest
        .split_once(", ")
        .unwrap_or_else(|| panic!("signed headers second"));
    assert_eq!(
        credential_pair,
        "Credential=AKIDEXAMPLE/20260115/us-east-1/bedrock/aws4_request"
    );
    assert_eq!(
        signed_headers,
        "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"
    );
    assert!(signature.starts_with("Signature="));
    assert_eq!(signature.len(), "Signature=".len() + 64);
}

#[test]
fn sign_is_deterministic_for_identical_inputs() {
    let credential = credential();
    let headers = [
        ("content-type", String::from("application/json")),
        ("accept", String::from("application/vnd.amazon.eventstream")),
    ];
    let build = || SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/m/converse-stream",
        query: None,
        headers: &headers,
        service: SERVICE,
        region: "us-east-1",
        credential: &credential,
        date: date("20260115T120000Z"),
    };
    let first = signed_auth(&build(), b"body");
    let second = signed_auth(&build(), b"body");
    assert_eq!(first, second);
    assert_ne!(first, signed_auth(&build(), b"other"));
}

#[test]
fn session_token_joins_the_signed_headers() {
    let mut session_credential = credential();
    session_credential.session_token = Some("AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9S".into());
    let request = SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/m/converse-stream",
        query: None,
        headers: &[("content-type", String::from("application/json"))],
        service: SERVICE,
        region: "us-east-1",
        credential: &session_credential,
        date: date("20260115T120000Z"),
    };
    let signed = request.sign(b"{}");
    assert_eq!(
        signed.security_token.as_deref(),
        Some("AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9S")
    );
    let signed_headers = signed
        .authorization
        .split_once("SignedHeaders=")
        .and_then(|(_, rest)| rest.split_once(','))
        .map_or_else(
            || panic!("authorization carries SignedHeaders"),
            |(headers, _)| headers,
        );
    assert_eq!(
        signed_headers, "content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token",
        "SignedHeaders are sorted, semicolon-separated, with no trailing separator (AWS SigV4)"
    );
    // The session token must shape the signature, not just the header list:
    // an identical request signed without the token signs different bytes,
    // so its signature differs.
    let plain = credential();
    let plain_request = SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/m/converse-stream",
        query: None,
        headers: &[("content-type", String::from("application/json"))],
        service: SERVICE,
        region: "us-east-1",
        credential: &plain,
        date: date("20260115T120000Z"),
    };
    let plain_signature = plain_request
        .sign(b"{}")
        .authorization
        .rsplit_once("Signature=")
        .map_or_else(
            || panic!("plain authorization carries a signature"),
            |(_, signature)| signature.to_owned(),
        );
    let token_signature = signed.authorization.rsplit_once("Signature=").map_or_else(
        || panic!("token authorization carries a signature"),
        |(_, signature)| signature,
    );
    assert_ne!(token_signature, plain_signature.as_str());
    assert!(
        !plain_request
            .sign(b"{}")
            .authorization
            .contains("x-amz-security-token")
    );
    // Rebuild the canonical request and string to sign by hand, from the
    // documented grammar, and check the emitted signature covers the token
    // header line.
    let payload_hash = sha256_hex(b"{}");
    let canonical_request = [
        "POST",
        "/model/m/converse-stream",
        "",
        &format!(
            "content-type:application/json\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:20260115T120000Z\nx-amz-security-token:AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9S\n",
            host = "bedrock-runtime.us-east-1.amazonaws.com",
        ),
        signed_headers,
        &payload_hash,
    ]
    .join("\n");
    let scope = "20260115/us-east-1/bedrock/aws4_request";
    let string_to_sign = [
        "AWS4-HMAC-SHA256",
        "20260115T120000Z",
        scope,
        &sha256_hex(canonical_request.as_bytes()),
    ]
    .join("\n");
    let key = signing_key(
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        "20260115",
        "us-east-1",
        "bedrock",
    );
    let expected = hex(&hmac_sha256(&key, string_to_sign.as_bytes()));
    assert!(
        signed
            .authorization
            .ends_with(&format!("Signature={expected}"))
    );
}

#[test]
fn signer_owned_and_unsolicited_headers_never_join_the_signature() {
    let credential = credential();
    let request = SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/m/converse-stream",
        query: None,
        headers: &[
            ("Content-Type", String::from("application/json")),
            ("User-Agent", String::from("dalgon/0.1.0")),
            ("X-Amz-Date", String::from("19700101T000000Z")),
            ("x-amzn-trace-id", String::from("Root=1-0")),
        ],
        service: SERVICE,
        region: "us-east-1",
        credential: &credential,
        date: date("20260115T120000Z"),
    };
    let signed = request.sign(b"{}");
    assert!(
        signed
            .authorization
            .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date")
    );
    assert!(!signed.authorization.contains("user-agent"));
    assert!(!signed.authorization.contains("x-amzn-trace-id"));
    // At the signature level the ignored headers change nothing, even a
    // caller `X-Amz-Date` naming another instant than the signer's own.
    let baseline = SignedRequest {
        headers: &[("Content-Type", String::from("application/json"))],
        ..request.clone()
    }
    .sign(b"{}");
    assert_eq!(signed.authorization, baseline.authorization);
    assert_eq!(signed, baseline);
}

#[test]
fn header_values_are_trimmed_and_folded_before_signing() {
    let credential = credential();
    let sign = |value: &str| {
        SignedRequest {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            path: "/model/m/converse-stream",
            query: None,
            headers: &[("Content-Type", String::from(value))],
            service: SERVICE,
            region: "us-east-1",
            credential: &credential,
            date: date("20260115T120000Z"),
        }
        .sign(b"{}")
        .authorization
    };
    let clean = sign("application/json");
    // Space and tab padding at either end signs the same bytes as no padding.
    for padded in [
        "  application/json",
        "application/json\t",
        " \t application/json \t ",
    ] {
        assert_eq!(sign(padded), clean, "{padded:?}");
    }
    // A different value still signs differently, so the equalities above are
    // not an artifact of the header being ignored.
    assert_ne!(sign("application/xml"), clean);
}

#[test]
fn debug_never_carries_the_secret() {
    let credential = credential();
    let rendered = format!("{credential:?}");
    assert!(!rendered.contains("AKIDEXAMPLE"));
    assert!(!rendered.contains("wJalrXUtnFEMI"));
}

#[test]
fn a_signed_header_set_never_prints_its_token_or_authorization() {
    let mut session_credential = credential();
    session_credential.session_token = Some("SESSION-TOKEN-CANARY".into());
    let request = SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/m/converse-stream",
        query: None,
        headers: &[("content-type", String::from("application/json"))],
        service: SERVICE,
        region: "us-east-1",
        credential: &session_credential,
        date: date("20260115T120000Z"),
    };
    let signed = request.sign(b"{}");
    let rendered = format!("{signed:?}");
    assert!(!rendered.contains("SESSION-TOKEN-CANARY"), "{rendered}");
    assert!(!rendered.contains("AKIDEXAMPLE"), "{rendered}");
    assert!(!rendered.contains(&signed.authorization), "{rendered}");
    assert!(rendered.contains("bedrock-runtime.us-east-1.amazonaws.com"));
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("SESSION-TOKEN-CANARY"), "{rendered}");
}

#[test]
fn a_credential_that_would_sign_differently_than_it_travels_is_refused() {
    for token in [
        "abc def", " abc", "abc ", "abc\tdef", "abc\ndef", "tökén", "",
    ] {
        let mut hostile = credential();
        hostile.session_token = Some(token.into());
        assert_eq!(
            hostile.verify_usable(0),
            Err(CredentialError::Unsignable {
                field: "session token"
            }),
            "token {token:?}"
        );
    }
    for key in ["AKID EXAMPLE", "AKID\r\nX-Injected: 1", "", "ключ"] {
        let mut hostile = credential();
        hostile.access_key_id = key.into();
        assert_eq!(
            hostile.verify_usable(0),
            Err(CredentialError::Unsignable {
                field: "access key id"
            }),
            "key id {key:?}"
        );
    }
    let mut padded = credential();
    padded.session_token = Some("AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9S+=".into());
    assert_eq!(padded.verify_usable(0), Ok(()));
    let message = CredentialError::Unsignable {
        field: "session token",
    }
    .to_string();
    assert!(message.contains("session token"), "{message}");
}

#[test]
fn fold_trims_ends_and_collapses_interior_runs() {
    assert_eq!(fold("  application/json  "), "application/json");
    assert_eq!(fold("a  b\t c   d"), "a b c d");
    assert_eq!(fold("single"), "single");
    assert_eq!(fold("   "), "");
    assert_eq!(fold("  a  "), "a");
}

#[test]
fn interior_whitespace_is_folded_into_the_signature() {
    let credential = credential();
    let folded = SignedRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/model/m/converse-stream",
        query: None,
        headers: &[("Content-Type", String::from("text/plain;  charset=utf-8"))],
        service: SERVICE,
        region: "us-east-1",
        credential: &credential,
        date: date("20260115T120000Z"),
    };
    let unfolded = SignedRequest {
        headers: &[("Content-Type", String::from("text/plain; charset=utf-8"))],
        ..folded.clone()
    };
    assert_eq!(
        folded.sign(b"{}"),
        unfolded.sign(b"{}"),
        "canonical folding signs the folded form"
    );
}

#[test]
fn from_unix_formats_the_amz_date_pair() {
    let date = AmzDate::from_unix(1_768_478_400);
    assert_eq!(date.long, "20260115T120000Z");
    assert_eq!(date.short, "20260115");
    let epoch = AmzDate::from_unix(0);
    assert_eq!(epoch.long, "19700101T000000Z");
    assert_eq!(epoch.short, "19700101");
}

#[test]
fn expired_credentials_are_refused_before_signing() {
    let mut expiring = credential();
    expiring.expires_at = Some(1_000);
    assert!(expiring.verify_usable(999).is_ok());
    // Expiry is checked before the characters, and names only the expiry.
    expiring.session_token = Some("not a token".into());
    assert_eq!(
        expiring.verify_usable(1_000),
        Err(CredentialError::Expired { expires_at: 1_000 })
    );
    expiring.session_token = None;
    let error = expiring
        .verify_usable(1_000)
        .err()
        .unwrap_or_else(|| panic!("at expiry the credential is spent"));
    assert_eq!(error, CredentialError::Expired { expires_at: 1_000 });
    assert!(expiring.verify_usable(1_001).is_err());

    let long_lived = credential();
    assert!(long_lived.verify_usable(i64::MAX).is_ok());
}
