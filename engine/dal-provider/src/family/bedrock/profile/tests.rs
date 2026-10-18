use super::*;

// Expected values come from `aws-runtime` 1.10.0 `env_config` and its
// `test-data/profile-parser-tests.json`; test names cite the corpus entry they
// mirror. Every test calls the one public decoder, `ProfileSet::parse`.

fn decode(config: &str, credentials: &str) -> ProfileSet {
    ProfileSet::parse(config, credentials).unwrap()
}

fn config_only(text: &str) -> ProfileSet {
    decode(text, "")
}

fn refusal(config: &str, credentials: &str) -> ProfileError {
    ProfileSet::parse(config, credentials).unwrap_err()
}

fn config_refusal(text: &str) -> ProfileError {
    refusal(text, "")
}

fn at(file: ProfileFile, line: usize, kind: ProfileErrorKind) -> ProfileError {
    ProfileError { file, line, kind }
}

fn value<'a>(set: &'a ProfileSet, profile: &str, key: &str) -> Option<&'a str> {
    set.profile(profile)?.get(key)
}

// ---- config and credentials merge ----------------------------------------

#[test]
fn credentials_value_beats_config_value_for_the_same_profile_and_key() {
    // "Duplicate properties between files uses credentials property."
    let set = decode("[profile foo]\nname = value", "[foo]\nname = value2");
    assert_eq!(value(&set, "foo", "name"), Some("value2"));
}

#[test]
fn one_profile_merges_key_by_key_across_both_files() {
    // "Profiles duplicated in multiple files are merged."
    let config = "[default]\nregion = us-west-2\noutput = json\n";
    let credentials = "[default]\naws_access_key_id = AKIDEXAMPLE\n\
                       aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n";
    let set = decode(config, credentials);
    assert_eq!(value(&set, "default", "region"), Some("us-west-2"));
    assert_eq!(value(&set, "default", "output"), Some("json"));
    assert_eq!(
        value(&set, "default", "aws_access_key_id"),
        Some("AKIDEXAMPLE")
    );
    assert_eq!(
        value(&set, "default", "aws_secret_access_key"),
        Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
    );
}

#[test]
fn credentials_only_and_config_only_profiles_both_survive() {
    let set = decode(
        "[profile from-config]\na = 1\n",
        "[from-credentials]\nb = 2\n",
    );
    assert_eq!(
        set.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["from-config", "from-credentials"]
    );
}

#[test]
fn config_sections_need_the_profile_prefix_and_credentials_sections_forbid_it() {
    // "Config profiles without prefix are ignored."
    assert_eq!(config_only("[foo]\nname = value\n"), ProfileSet::default());
    // A header cut at `#` is still a bare name: ignored, not an error.
    assert_eq!(config_only("[SECRET-12] # c\n"), ProfileSet::default());
    // "Credentials profiles with prefix are ignored."
    assert_eq!(
        decode("", "[profile foo]\nname = value\n"),
        ProfileSet::default()
    );
}

#[test]
fn ignored_unprefixed_config_section_does_not_hide_the_prefixed_one() {
    let set = config_only("[dev]\na = 1\n[profile dev]\nb = 2\n");
    assert_eq!(value(&set, "dev", "a"), None);
    assert_eq!(value(&set, "dev", "b"), Some("2"));
}

#[test]
fn services_sso_session_and_foreign_sections_are_ignored_in_credentials() {
    let credentials = "[sso-session corp]\na = 1\n[services s]\nd =\n  endpoint_url = http://x\n\
                       [other thing]\nb = 2\n";
    assert_eq!(decode("", credentials), ProfileSet::default());
}

#[test]
fn foreign_prefixed_config_sections_are_ignored() {
    // "A typo'd sso-session will be ignored": any other prefix is dropped.
    let set = config_only("[sso-sesion foo]\nname = value\n[profile bar]\nname2 = value2\n");
    assert_eq!(
        set.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["bar"]
    );
    assert!(set.sso_session("foo").is_none());
}

#[test]
fn parse_is_deterministic_for_equal_input() {
    let config = "[profile b]\nk = 1\n[profile a]\nk = 2\n[default]\nk = 3\n";
    assert_eq!(
        decode(config, "[a]\nk = 4\n"),
        decode(config, "[a]\nk = 4\n")
    );
}

// ---- [default] and [profile default] -------------------------------------

#[test]
fn prefixed_default_replaces_bare_default_whichever_comes_first() {
    // "Default profile with profile prefix overrides default profile without
    // prefix when profile prefix is first / last."
    let first = config_only("[profile default]\nname = value\n[default]\nname2 = value2\n");
    let last = config_only("[default]\nname2 = value2\n[profile default]\nname = value\n");
    for set in [&first, &last] {
        assert_eq!(value(set, "default", "name"), Some("value"));
        assert_eq!(value(set, "default", "name2"), None);
    }
}

#[test]
fn prefixed_default_blocks_the_bare_default_but_still_merges_with_credentials() {
    // "Default profiles with mixed prefixes merge with credentials"
    let config = "[profile default]\nname = value\n[default]\nname2 = value2\n\
                  [profile default]\nname3 = value3\n";
    let set = decode(config, "[default]\nsecret=foo\n");
    assert_eq!(value(&set, "default", "name"), Some("value"));
    assert_eq!(value(&set, "default", "name2"), None);
    assert_eq!(value(&set, "default", "name3"), Some("value3"));
    assert_eq!(value(&set, "default", "secret"), Some("foo"));
}

#[test]
fn bare_default_is_the_default_profile_when_no_prefixed_default_exists() {
    let set = config_only("[default]\nregion = us-east-1\n");
    assert_eq!(value(&set, "default", "region"), Some("us-east-1"));
}

#[test]
fn prefixed_default_with_extra_whitespace_still_replaces_bare_default() {
    let set = config_only("[profile   default]\na = 1\n[default]\nb = 2\n");
    assert_eq!(value(&set, "default", "a"), Some("1"));
    assert_eq!(value(&set, "default", "b"), None);
}

#[test]
fn default_in_a_different_case_is_a_separate_profile() {
    let set = config_only("[profile Default]\na = 1\n[default]\nb = 2\n");
    assert_eq!(value(&set, "Default", "a"), Some("1"));
    assert_eq!(value(&set, "Default", "b"), None);
    assert_eq!(value(&set, "default", "b"), Some("2"));
    assert_eq!(value(&set, "default", "a"), None);
}

// ---- names and case ------------------------------------------------------

#[test]
fn profile_names_are_case_sensitive() {
    let set = decode(
        "[profile Dev]\nregion = us-east-1\n[profile dev]\nregion = eu-west-1\n",
        "",
    );
    assert_eq!(value(&set, "Dev", "region"), Some("us-east-1"));
    assert_eq!(value(&set, "dev", "region"), Some("eu-west-1"));
    assert!(set.profile("DEV").is_none());
    let credentials = decode("", "[Prod]\nregion = a\n");
    assert!(credentials.profile("prod").is_none());
    assert!(credentials.profile("Prod").is_some());
}

#[test]
fn property_keys_are_ascii_case_insensitive_and_the_last_one_wins() {
    // "Duplicate properties in duplicate profiles use the last one defined
    // (case insensitive)."
    let set = config_only("[profile foo]\nName = value\nname = value2\nAWS_ACCESS_KEY_ID = AKID\n");
    assert_eq!(value(&set, "foo", "name"), Some("value2"));
    assert_eq!(value(&set, "foo", "NAME"), Some("value2"));
    assert_eq!(value(&set, "foo", "Aws_Access_Key_Id"), Some("AKID"));
}

#[test]
fn non_ascii_keys_are_not_case_folded_and_are_dropped_as_invalid() {
    // `to_ascii_lowercase` keeps non-ASCII bytes, so `K\u{c9}Y` becomes
    // `k\u{c9}y`, which is not an identifier: the property is ignored.
    let set = config_only("[profile foo]\nK\u{c9}Y = upper\nok = 1\n");
    assert_eq!(
        set.profile("foo")
            .unwrap()
            .properties
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["ok"]
    );
}

#[test]
fn profile_name_alphabet_is_the_identifier_alphabet() {
    // "Profile names support alphanumeric characters and - / . % @ _ : +"
    let set = config_only("[profile some-thing:long/the_one%only.foo@bar+]\n[profile foo!bar]\n");
    assert!(
        set.profile("some-thing:long/the_one%only.foo@bar+")
            .is_some()
    );
    assert!(set.profile("foo!bar").is_none());
}

#[test]
fn invalid_profile_names_are_ignored_and_their_properties_do_not_leak() {
    // "properties from an invalid profile name are ignored"
    let config = "[profile foo]\nname = value\n[profile in valid]\nx = 1\n[profile bar]\n\
                  name = value2\n";
    let set = decode(config, "[in valid 2]\nname2 = value2\n");
    assert_eq!(
        set.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["bar", "foo"]
    );
    assert_eq!(value(&set, "foo", "x"), None);
    assert_eq!(value(&set, "bar", "name"), Some("value2"));
}

#[test]
fn invalid_property_names_are_ignored() {
    // "Invalid property names are ignored."
    let set = config_only("[profile foo]\nin valid = value\nok = 1\n");
    assert_eq!(
        set.profile("foo")
            .unwrap()
            .properties
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["ok"]
    );
}

#[test]
fn whitespace_inside_section_headers_is_trimmed() {
    // "Profile names should be trimmed.", "Tabs can separate profile names
    // from profile prefix.", "profile name with extra whitespace"
    let config = "[   profile foo    ]\nname = value\n[profile \tbar \t]\n[profile\tbaz]\n";
    let set = decode(config, "[   cred    ]\nname = value\n");
    assert_eq!(value(&set, "foo", "name"), Some("value"));
    assert!(set.profile("bar").is_some());
    assert!(set.profile("baz").is_some());
    assert_eq!(value(&set, "cred", "name"), Some("value"));
}

#[test]
fn sneaky_prefixes_without_a_separating_space_are_not_prefixes() {
    // "sneaky profile name", "sso-session without the space before the name
    // is a profile"
    let set = decode(
        "[profilefoo]\nname = value\n[profile bar]\n",
        "[sso-sessionfoo]\nn = v\n",
    );
    assert_eq!(
        set.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["bar", "sso-sessionfoo"]
    );
}

#[test]
fn a_section_named_like_a_prefix_is_still_a_profile_name() {
    let config =
        "[profile profile]\na = 1\n[profile sso-session]\nb = 2\n[profile services]\nc = 3\n";
    let set = config_only(config);
    assert_eq!(
        set.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["profile", "services", "sso-session"]
    );
}

#[test]
fn empty_profiles_exist_without_properties() {
    // "Empty profiles have no properties.", "Multiple profiles can be empty."
    let set = config_only("[profile foo]\n[profile bar]");
    assert_eq!(set.profile("foo").unwrap().properties.len(), 0);
    assert!(set.profile("bar").is_some());
    assert_eq!(config_only(""), ProfileSet::default());
}

// ---- duplicate sections and keys -----------------------------------------

#[test]
fn duplicate_sections_in_one_file_merge_and_the_later_key_wins() {
    // "Duplicate profiles in the same file merge properties.", "Duplicate
    // properties in duplicate profiles use the last one defined."
    let config = "[profile foo]\nname = value\n[profile foo]\nname2 = value2\n\
                  [profile foo]\nname = value3\n";
    let set = config_only(config);
    assert_eq!(value(&set, "foo", "name"), Some("value3"));
    assert_eq!(value(&set, "foo", "name2"), Some("value2"));
}

#[test]
fn duplicate_keys_in_one_section_keep_the_last_value() {
    // "Duplicate properties in a profile use the last one defined."
    let set = config_only("[profile foo]\nname = value\nname = value2\n");
    assert_eq!(value(&set, "foo", "name"), Some("value2"));
}

#[test]
fn headers_that_differ_only_in_spelling_apply_in_first_appearance_order() {
    // The reference orders these by `HashMap` iteration; this decoder applies
    // them in the order their first spelling appears, so the later one wins.
    let set = config_only("[profile foo]\nk = a\n[profile  foo]\nk = b\n");
    assert_eq!(value(&set, "foo", "k"), Some("b"));
    let reversed = config_only("[profile  foo]\nk = b\n[profile foo]\nk = a\n");
    assert_eq!(value(&reversed, "foo", "k"), Some("a"));
}

#[test]
fn a_later_duplicate_property_replaces_the_earlier_value_and_lines() {
    let config = "[profile foo]\ns3 =\n  a = 1\ns3 = flat\n";
    let set = config_only(config);
    assert_eq!(value(&set, "foo", "s3"), Some("flat"));
    assert_eq!(set.profiles["foo"].properties["s3"].nested.get("a"), None);
}

#[test]
fn a_credentials_property_replaces_the_whole_config_property() {
    let set = decode("[profile foo]\ns3 =\n  a = 1\n", "[foo]\ns3 = flat\n");
    assert_eq!(value(&set, "foo", "s3"), Some("flat"));
    assert_eq!(set.profiles["foo"].properties["s3"].nested.get("a"), None);
}

// ---- values: comments, quotes, backslashes, URLs, ARNs --------------------

#[test]
fn comment_characters_without_leading_whitespace_stay_in_the_value() {
    // "Comments adjacent to values are included in the value."
    let config = "[default]\naws_secret_access_key = aB3#dE/f;9=\nname = value; Adjacent semicolons\n\
                  name2 = value# Adjacent pound signs\n";
    let set = config_only(config);
    assert_eq!(
        value(&set, "default", "aws_secret_access_key"),
        Some("aB3#dE/f;9=")
    );
    assert_eq!(
        value(&set, "default", "name"),
        Some("value; Adjacent semicolons")
    );
    assert_eq!(
        value(&set, "default", "name2"),
        Some("value# Adjacent pound signs")
    );
}

#[test]
fn a_comment_after_whitespace_is_removed_from_the_value() {
    // "Pound sign comments are ignored.", "Semicolon sign comments are
    // ignored.", "Comments can be empty."
    let config = "[default]\nname = value # Comment with # sign\nname2 = value ; Comment with ; sign\n\
                  name3 = value\t# tab\nname4 = value ;\n";
    let set = config_only(config);
    for key in ["name", "name2", "name3", "name4"] {
        assert_eq!(value(&set, "default", key), Some("value"), "{key}");
    }
}

#[test]
fn comments_can_be_adjacent_to_headers_and_values_together() {
    // "All comment types can be used together.", "Comments can be adjacent to
    // profile names."
    let config = "# Comment\n[profile foo] ; Comment\nname = value # Comment with ; sign\n\
                  [profile bar]# Adjacent pound signs\n[profile baz]; Adjacent semicolons\n";
    let set = config_only(config);
    assert_eq!(value(&set, "foo", "name"), Some("value"));
    assert!(set.profile("bar").is_some());
    assert!(set.profile("baz").is_some());
}

#[test]
fn full_line_comments_and_blank_lines_are_skipped() {
    // "Blank lines are ignored."
    let config =
        "# Comment\n; Comment\n\n[profile foo]\n# inside\nname = value\n\t \n[profile bar]\n \t";
    let set = config_only(config);
    assert_eq!(value(&set, "foo", "name"), Some("value"));
    assert_eq!(set.profile("bar").unwrap().properties.len(), 0);
}

#[test]
fn a_value_that_starts_with_a_comment_character_without_whitespace_is_literal() {
    let set = config_only("[profile foo]\nname =# literal\nname2 =;also\n");
    assert_eq!(value(&set, "foo", "name"), Some("# literal"));
    assert_eq!(value(&set, "foo", "name2"), Some(";also"));
}

#[test]
fn a_value_that_starts_with_a_comment_after_whitespace_is_an_empty_value() {
    // "Comments can be empty.": `k = # c` reads as an empty value.
    let set = config_only("[profile foo]\nname = # c\nname2 =  ; c\n");
    assert_eq!(value(&set, "foo", "name"), Some(""));
    assert_eq!(value(&set, "foo", "name2"), Some(""));
}

#[test]
fn empty_values_are_kept_empty() {
    // "Property values can be empty."
    let set = config_only("[profile foo]\nname =\nname2 =   \n");
    assert_eq!(value(&set, "foo", "name"), Some(""));
    assert_eq!(value(&set, "foo", "name2"), Some(""));
}

#[test]
fn keys_and_values_are_trimmed_of_spaces_and_tabs() {
    // "Property keys and values are trimmed."
    let set = config_only("[profile foo]\nname \t=  \tvalue \t\n  \n");
    assert_eq!(value(&set, "foo", "name"), Some("value"));
}

#[test]
fn quotes_and_backslashes_survive_in_a_credential_process_command() {
    let config = r#"[profile p]
credential_process = /usr/local/bin/my-helper --profile "prod user" --tag 'a b' --path C:\new\table\x41
"#;
    let set = config_only(config);
    assert_eq!(
        value(&set, "p", "credential_process"),
        Some(
            r#"/usr/local/bin/my-helper --profile "prod user" --tag 'a b' --path C:\new\table\x41"#
        )
    );
}

#[test]
fn a_backslash_line_join_is_not_applied_and_backslashes_stay_verbatim() {
    // `aws-runtime` has no backslash continuation: `value\` keeps the
    // backslash and `name2` stays its own property.
    let set = config_only("[profile foo]\nname = value\\\nname2 = other\n");
    assert_eq!(value(&set, "foo", "name"), Some("value\\"));
    assert_eq!(value(&set, "foo", "name2"), Some("other"));
    let set = config_only(
        "[default]\r\ncredential_process = C:\\tools\\run.bat\\\r\nregion = us-east-1\r\n",
    );
    assert_eq!(
        value(&set, "default", "credential_process"),
        Some("C:\\tools\\run.bat\\")
    );
    assert_eq!(value(&set, "default", "region"), Some("us-east-1"));
}

#[test]
fn a_backslash_at_the_end_of_the_input_stays_in_the_value() {
    let set = config_only("[profile foo]\nname = value\\");
    assert_eq!(value(&set, "foo", "name"), Some("value\\"));
}

#[test]
fn a_backslash_in_a_section_name_or_key_is_an_invalid_name_not_a_delimiter_eater() {
    // The reference keeps the backslash in the name; the name fails the
    // identifier alphabet, and every other line still decodes.
    let set = config_only("[profile a\\]\nk = v\nnext = 1\n[profile b]\nk = w\n");
    assert_eq!(
        set.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["b"]
    );
    assert_eq!(value(&set, "b", "k"), Some("w"));
    let set = config_only("[profile foo]\nfoo\\=bar\nnext = 1\n");
    assert_eq!(set.profile("foo").unwrap().properties.len(), 1);
    assert_eq!(value(&set, "foo", "next"), Some("1"));
}

#[test]
fn a_colon_inside_a_header_or_a_key_is_a_name_character() {
    // `[profile a;b]` is a reference error (`prepare_line(line, false)` cuts
    // at the `;`, so `]` is gone); `[profile a]b]` is an ignored name. A
    // colon-key line keeps the key whole, split at the `=`.
    assert_eq!(
        config_refusal("[profile a;b]\n"),
        at(
            ProfileFile::Config,
            1,
            ProfileErrorKind::UnterminatedSectionHeader
        )
    );
    let set = config_only("[profile a]b]\nname = value\n");
    assert_eq!(set, ProfileSet::default());
    let set = config_only("[profile foo]\nx:y = z\n");
    assert_eq!(value(&set, "foo", "x:y"), Some("z"));
}

#[test]
fn a_colon_is_not_a_delimiter_on_a_property_line() {
    // "Property definitions must contain an equals sign."
    assert_eq!(
        config_refusal("[profile foo]\nkey : value\n"),
        at(ProfileFile::Config, 2, ProfileErrorKind::MissingEquals)
    );
    assert_eq!(
        config_refusal("[profile foo]\nkey: value\n"),
        at(ProfileFile::Config, 2, ProfileErrorKind::MissingEquals)
    );
}

#[test]
fn a_secret_with_every_delimiter_and_quote_character_is_kept_verbatim() {
    let config = "[default]\naws_secret_access_key = a#b;c=d:e\\f\"g'h==\n";
    let set = decode("", config);
    assert_eq!(
        value(&set, "default", "aws_secret_access_key"),
        Some("a#b;c=d:e\\f\"g'h==")
    );
}

#[test]
fn urls_arns_and_paths_keep_colons_slashes_and_fragments() {
    let config = "[profile p]\n\
        role_arn = arn:aws:iam::123456789012:role/Admin\n\
        mfa_serial = arn:aws:iam::123456789012:mfa/user\n\
        web_identity_token_file = /var/run/secrets/eks.amazonaws.com/serviceaccount/token\n\
        sso_start_url = https://portal.example.awsapps.com/start#/\n\
        sso_registration_scopes = sso:account:access\n\
        endpoint_url = http://localhost:4566/path;x=y?a=b&c=d#frag\n";
    let set = config_only(config);
    assert_eq!(
        value(&set, "p", "role_arn"),
        Some("arn:aws:iam::123456789012:role/Admin")
    );
    assert_eq!(
        value(&set, "p", "mfa_serial"),
        Some("arn:aws:iam::123456789012:mfa/user")
    );
    assert_eq!(
        value(&set, "p", "web_identity_token_file"),
        Some("/var/run/secrets/eks.amazonaws.com/serviceaccount/token")
    );
    assert_eq!(
        value(&set, "p", "sso_start_url"),
        Some("https://portal.example.awsapps.com/start#/")
    );
    assert_eq!(
        value(&set, "p", "sso_registration_scopes"),
        Some("sso:account:access")
    );
    assert_eq!(
        value(&set, "p", "endpoint_url"),
        Some("http://localhost:4566/path;x=y?a=b&c=d#frag")
    );
}

#[test]
fn equals_signs_and_base64_padding_in_values_are_kept() {
    // "Equals signs are supported in property values."
    let set = config_only("[profile foo]\nname = val=ue\ntoken = aGVsbG8=\npad == x\n");
    assert_eq!(value(&set, "foo", "name"), Some("val=ue"));
    assert_eq!(value(&set, "foo", "token"), Some("aGVsbG8="));
    assert_eq!(value(&set, "foo", "pad"), Some("= x"));
}

#[test]
fn unicode_values_and_unicode_whitespace_edges_are_kept() {
    // "Unicode characters are supported in property values." Only space and
    // tab are trimmed; U+00A0 is part of the value, and interior whitespace
    // is kept everywhere.
    let set = config_only(
        "[profile foo]\nname = \u{1F602} \u{65E5}\u{672C}\nnbsp = a\u{a0}b\ntrail = v\u{a0}\n",
    );
    assert_eq!(
        value(&set, "foo", "name"),
        Some("\u{1F602} \u{65E5}\u{672C}")
    );
    assert_eq!(value(&set, "foo", "nbsp"), Some("a\u{a0}b"));
    assert_eq!(value(&set, "foo", "trail"), Some("v\u{a0}"));
}

#[test]
fn crlf_line_endings_decode_like_lf() {
    // "Windows style line endings are supported."
    let config = "[profile foo]\r\nname = value\r\n# c\r\nname2 = value2\r\n\r\n[profile bar]\r\n";
    let set = config_only(config);
    assert_eq!(value(&set, "foo", "name"), Some("value"));
    assert_eq!(value(&set, "foo", "name2"), Some("value2"));
    assert!(set.profile("bar").is_some());
}

#[test]
fn a_carriage_return_inside_a_line_is_part_of_the_line() {
    // `lines()` splits on `\n` and strips one `\r` only before it; a bare
    // `\r` stays in the value.
    let set = config_only("[profile foo]\nname = a\rb\n");
    assert_eq!(value(&set, "foo", "name"), Some("a\rb"));
}

#[test]
fn a_byte_order_mark_makes_the_first_line_a_property_outside_a_section() {
    // The reference does not strip a BOM: line 1 is not a header, not blank,
    // and not indented, so it fails as a property before any section
    // (source.rs:68-76, parse.rs:145).
    let text = "\u{FEFF}[default]\nname = value\n";
    assert_eq!(
        config_refusal(text),
        at(
            ProfileFile::Config,
            1,
            ProfileErrorKind::PropertyBeforeSection
        )
    );
    assert_eq!(
        refusal("", text),
        at(
            ProfileFile::Credentials,
            1,
            ProfileErrorKind::PropertyBeforeSection
        )
    );
    // Only the first character is special.
    let interior = config_only("[default]\nname = a\u{FEFF}b\n");
    assert_eq!(value(&interior, "default", "name"), Some("a\u{FEFF}b"));
}

// ---- continuations and sub-properties -------------------------------------

#[test]
fn a_value_continues_on_the_next_line_joined_with_a_newline() {
    // "Property values can be continued on the next line.", "...with multiple
    // lines.", "Continuations are trimmed."
    let set = config_only(
        "[profile foo]\nname = value\n -continued\nname2 = value\n -a\n -b\nname3 = value\n \t -c \t \n",
    );
    assert_eq!(value(&set, "foo", "name"), Some("value\n-continued"));
    assert_eq!(value(&set, "foo", "name2"), Some("value\n-a\n-b"));
    assert_eq!(value(&set, "foo", "name3"), Some("value\n-c"));
}

#[test]
fn continuation_values_include_comments() {
    // "Continuation values include pound comments.", "...semicolon comments."
    let set = config_only("[profile foo]\nname = value\n -continued # Comment\n");
    assert_eq!(
        value(&set, "foo", "name"),
        Some("value\n-continued # Comment")
    );
}

#[test]
fn an_indented_comment_line_and_a_nested_line_below_it_attach_in_order() {
    // Continuations append `trim_matches(WHITESPACE)` without comment
    // stripping; an indented `#` line is a continuation, not a comment
    // (parse.rs:79-81, parse.rs:188-199).
    let set = config_only("[profile foo]\nname = value\n  # note = kept\n  more = 1\n");
    assert_eq!(
        value(&set, "foo", "name"),
        Some("value\n# note = kept\nmore = 1")
    );
}

#[test]
fn an_empty_first_line_value_is_returned_whole_and_exposes_sub_properties() {
    // "Properties can contain sub-properties." The reference keeps the whole
    // newline-joined text (`"s3": "\nname = value"` in the corpus) and each
    // continuation is additionally exposed as a sub-property line.
    let set = config_only("[profile foo]\ns3 =\n name = value\n name2 = value2\n");
    assert_eq!(
        value(&set, "foo", "s3"),
        Some("\nname = value\nname2 = value2")
    );
    let s3 = &set.profiles["foo"].properties["s3"];
    assert_eq!(s3.nested.get("name").map(String::as_str), Some("value"));
    assert_eq!(s3.nested.get("name2").map(String::as_str), Some("value2"));
}

#[test]
fn sub_property_definitions_can_have_an_empty_value() {
    // "Sub-property definitions can have an empty value."
    let set = config_only("[profile foo]\ns3 =\n name =\n");
    assert_eq!(
        set.profiles["foo"].properties["s3"]
            .nested
            .get("name")
            .map(String::as_str),
        Some("")
    );
}

#[test]
fn a_sub_property_value_may_hold_its_own_equals_signs() {
    let set = config_only("[services s]\ndynamodb =\n  endpoint_url = http://a?b=c&d=e\n");
    assert_eq!(
        set.services["s"].services["dynamodb"]["endpoint_url"],
        "http://a?b=c&d=e"
    );
}

#[test]
fn an_invalid_sub_property_definition_is_a_reference_error() {
    // "Invalid sub-property definitions cause an error.", "Sub-property
    // definitions cannot have an empty name."
    assert_eq!(
        config_refusal("[profile foo]\ns3 =\n invalid\n"),
        at(
            ProfileFile::Config,
            3,
            ProfileErrorKind::SubPropertyMissingEquals
        )
    );
    assert_eq!(
        config_refusal("[profile foo]\ns3 =\n = value\n"),
        at(
            ProfileFile::Config,
            3,
            ProfileErrorKind::SubPropertyEmptyName
        )
    );
}

#[test]
fn sub_properties_can_have_blank_lines_between_them() {
    // "Sub-properties can have blank lines that are ignored"
    let set = config_only("[profile foo]\ns3 =\n name = value\n\t \n name2 = value2\n");
    let s3 = &set.profiles["foo"].properties["s3"];
    assert_eq!(s3.nested.get("name").map(String::as_str), Some("value"));
    assert_eq!(s3.nested.get("name2").map(String::as_str), Some("value2"));
}

#[test]
fn continuations_reset_at_every_section_header() {
    // "Continuations reset with profile definitions." Even a header of a
    // section that already exists clears the current property.
    assert_eq!(
        config_refusal("[profile foo]\nname = value\n[profile foo]\n -continued\n"),
        at(
            ProfileFile::Config,
            4,
            ProfileErrorKind::ContinuationWithoutProperty
        )
    );
}

#[test]
fn a_nested_line_under_a_non_empty_parent_is_appended_verbatim() {
    // The reference appends the raw text with no validation; the value keeps
    // the `=` of the continuation line.
    let set = config_only("[profile foo]\nname = value\n  continued = yes\n");
    assert_eq!(value(&set, "foo", "name"), Some("value\ncontinued = yes"));
    // The same after a blank line and a comment line.
    let set = config_only("[profile foo]\nname = value\n\n# note\n  tail\n");
    assert_eq!(value(&set, "foo", "name"), Some("value\ntail"));
}

// ---- services --------------------------------------------------------------

#[test]
fn services_section_decodes_endpoint_urls() {
    let set =
        config_only("[services my-services]\ndynamodb =\n  endpoint_url = http://localhost:8000\n");
    assert_eq!(
        set.services["my-services"].services["dynamodb"]["endpoint_url"],
        "http://localhost:8000"
    );
}

#[test]
fn services_names_fold_case_but_sub_property_keys_do_not() {
    let set = config_only("[services s]\nDynamoDB =\n  Endpoint_Url = http://x\n");
    let section = &set.services["s"];
    let dynamodb = &section.services["dynamodb"];
    assert_eq!(
        dynamodb.get("Endpoint_Url").map(String::as_str),
        Some("http://x")
    );
    assert_eq!(dynamodb.get("endpoint_url"), None);
    assert!(!section.services.contains_key("DynamoDB"));
    assert!(!set.services.contains_key("S"));
}

#[test]
fn services_first_line_text_with_an_equals_is_a_sub_property_line() {
    let set = config_only("[services s]\ndynamodb = endpoint_url = http://x\n");
    assert_eq!(
        set.services["s"].services["dynamodb"]["endpoint_url"],
        "http://x"
    );
}

#[test]
fn services_group_with_an_invalid_name_is_ignored() {
    let config = "[services s]\nbad name =\n  endpoint_url = http://x\ndynamodb =\n  endpoint_url = http://y\n";
    let set = config_only(config);
    assert!(!set.services["s"].services.contains_key("bad name"));
    assert_eq!(
        set.services["s"].services["dynamodb"]["endpoint_url"],
        "http://y"
    );
}

#[test]
fn identical_services_headers_share_a_section_and_merge_sub_properties() {
    // Identical headers share one raw section (parse.rs:210-212) and a later
    // property line replaces the whole earlier value (parse.rs:158), so the
    // replaced service's earlier sub-properties are gone; distinct spellings
    // stay separate sections and merge per key.
    let config = "[services s]\ndynamodb =\n  endpoint_url = http://a\n  other = 1\n\
                  [services s]\ndynamodb =\n  endpoint_url = http://b\n";
    let set = config_only(config);
    assert_eq!(
        set.services["s"].services["dynamodb"]["endpoint_url"],
        "http://b"
    );
    assert_eq!(set.services["s"].services["dynamodb"].get("other"), None);
    let config = "[services s]\ndynamodb =\n  endpoint_url = http://a\n\
                  [services  s]\ndynamodb =\n  endpoint_url = http://b\n";
    let set = config_only(config);
    assert_eq!(
        set.services["s"].services["dynamodb"]["endpoint_url"],
        "http://b"
    );
}

#[test]
fn continuation_values_of_services_keep_their_comments() {
    // "Continuation values include pound comments."
    let set = config_only("[services s]\ndynamodb =\n  endpoint_url = http://x # note\n");
    assert_eq!(
        set.services["s"].services["dynamodb"]["endpoint_url"],
        "http://x # note"
    );
}

#[test]
fn an_indented_comment_like_line_with_an_equals_is_a_sub_property() {
    let set = config_only("[services s]\ndynamodb =\n  # a = b\n");
    assert_eq!(set.services["s"].services["dynamodb"]["# a"], "b");
}

// ---- sso-session -----------------------------------------------------------

#[test]
fn sso_session_sections_are_typed_apart_from_profiles() {
    let config = "[sso-session corp]\nsso_start_url = https://corp.awsapps.com/start\n\
                  sso_region = us-east-1\nsso_registration_scopes = sso:account:access\n\
                  [profile dev]\nsso_session = corp\n";
    let set = config_only(config);
    let session = set.sso_session("corp").unwrap();
    assert_eq!(
        session.get("sso_start_url"),
        Some("https://corp.awsapps.com/start")
    );
    assert_eq!(session.get("SSO_REGION"), Some("us-east-1"));
    assert_eq!(
        session.get("sso_registration_scopes"),
        Some("sso:account:access")
    );
    assert!(set.profile("corp").is_none());
    assert!(set.sso_session("dev").is_none());
    assert_eq!(value(&set, "dev", "sso_session"), Some("corp"));
}

// ---- a whole realistic pair of files ---------------------------------------

#[test]
fn a_realistic_pair_of_files_decodes_completely() {
    let config = "# ~/.aws/config\n\
        [default]\n\
        region = us-east-1\n\
        output = json ; machine readable\n\
        \n\
        [profile dev]\n\
        role_arn = arn:aws:iam::123456789012:role/Dev\n\
        source_profile = default\n\
        credential_process = \"C:\\Program Files\\helper\\get.exe\" --profile \"dev user\"\n\
        s3 =\n\
        \x20 max_concurrent_requests = 20\n\
        \x20 max_queue_size = 10000\n\
        \n\
        [profile sso]\n\
        sso_session = corp\n\
        sso_account_id = 123456789012\n\
        sso_role_name = Admin\n\
        \n\
        [sso-session corp]\n\
        sso_start_url = https://corp.awsapps.com/start\n\
        sso_region = us-east-1\n\
        \n\
        [services local]\n\
        bedrock-runtime =\n\
        \x20 endpoint_url = http://localhost:4566\n";
    let credentials = "[default]\n\
        aws_access_key_id = AKIDEXAMPLE\n\
        aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n\
        aws_session_token = IQoJb3JpZ2luX2VjEXAMPLE//////////wEaCXVzLWVhc3QtMSJHMEUCIQ==\n";
    let set = decode(config, credentials);
    assert_eq!(value(&set, "default", "output"), Some("json"));
    assert_eq!(
        value(&set, "default", "aws_access_key_id"),
        Some("AKIDEXAMPLE")
    );
    assert_eq!(
        value(&set, "default", "aws_session_token"),
        Some("IQoJb3JpZ2luX2VjEXAMPLE//////////wEaCXVzLWVhc3QtMSJHMEUCIQ==")
    );
    assert_eq!(
        value(&set, "dev", "credential_process"),
        Some("\"C:\\Program Files\\helper\\get.exe\" --profile \"dev user\"")
    );
    assert_eq!(
        set.profiles["dev"].properties["s3"]
            .nested
            .get("max_queue_size")
            .map(String::as_str),
        Some("10000")
    );
    assert_eq!(value(&set, "sso", "sso_role_name"), Some("Admin"));
    assert_eq!(
        set.sso_session("corp").unwrap().get("sso_region"),
        Some("us-east-1")
    );
    assert_eq!(
        set.services["local"].services["bedrock-runtime"]["endpoint_url"],
        "http://localhost:4566"
    );
}

// ---- inputs the reference rejects; the decoder rejects them too -----------

#[test]
fn a_property_before_any_section_is_rejected() {
    // "Properties must be defined in a profile."
    assert_eq!(
        config_refusal("name = value\n"),
        at(
            ProfileFile::Config,
            1,
            ProfileErrorKind::PropertyBeforeSection
        )
    );
    assert_eq!(
        refusal("", "# c\n\nname = value\n"),
        at(
            ProfileFile::Credentials,
            3,
            ProfileErrorKind::PropertyBeforeSection
        )
    );
}

#[test]
fn a_continuation_outside_a_profile_or_a_property_is_rejected() {
    // "Continuations cannot be used outside of a profile.", "...outside of a
    // property."
    assert_eq!(
        config_refusal(" -continued"),
        at(
            ProfileFile::Config,
            1,
            ProfileErrorKind::PropertyBeforeSection
        )
    );
    assert_eq!(
        config_refusal("[profile foo]\n -continued"),
        at(
            ProfileFile::Config,
            2,
            ProfileErrorKind::ContinuationWithoutProperty
        )
    );
}

#[test]
fn an_unterminated_section_header_is_rejected_not_swallowed() {
    // "Profile definitions must end with brackets." A stray `]` later does
    // not close the header: the line is processed as a whole.
    assert_eq!(
        config_refusal("[profile foo\nname = value\n[profile bar]\n"),
        at(
            ProfileFile::Config,
            1,
            ProfileErrorKind::UnterminatedSectionHeader
        )
    );
}

#[test]
fn a_bare_key_is_rejected_on_its_own_line() {
    assert_eq!(
        config_refusal("[profile foo]\nvalueless_key\nname = value\n"),
        at(ProfileFile::Config, 2, ProfileErrorKind::MissingEquals)
    );
    assert_eq!(
        config_refusal("[profile foo]\nvalueless_key"),
        at(ProfileFile::Config, 2, ProfileErrorKind::MissingEquals)
    );
}

#[test]
fn an_empty_property_name_is_rejected() {
    // "Property key cannot be empty."
    assert_eq!(
        config_refusal("[profile foo]\n= value\n"),
        at(ProfileFile::Config, 2, ProfileErrorKind::EmptyPropertyName)
    );
}

#[test]
fn an_indented_header_without_a_property_above_it_is_rejected() {
    // The reference reads it as a continuation, and there is no property.
    assert_eq!(
        config_refusal("[profile foo]\n  [profile bar]\n"),
        at(
            ProfileFile::Config,
            2,
            ProfileErrorKind::ContinuationWithoutProperty
        )
    );
}

#[test]
fn the_first_refused_line_wins_and_the_file_is_named() {
    let config = "[default]\nregion = x\n";
    assert_eq!(
        refusal(config, "[default]\nok = 1\nbad line\nworse line\n"),
        at(ProfileFile::Credentials, 3, ProfileErrorKind::MissingEquals)
    );
    assert_eq!(
        refusal("[default]\nbad line\n", "[default]\nalso bad\n"),
        at(ProfileFile::Config, 2, ProfileErrorKind::MissingEquals)
    );
}

// ---- redaction -----------------------------------------------------------

#[test]
fn errors_never_contain_text_from_the_input() {
    let config = "[default]\naws_secret_access_key = SECRET-VALUE-9f3\nSECRET-BARE-KEY-77\n";
    let error = config_refusal(config);
    assert_eq!(
        error,
        at(ProfileFile::Config, 3, ProfileErrorKind::MissingEquals)
    );
    let shown = format!("{error} | {error:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert_eq!(
        error.to_string(),
        "AWS config file, line 3: Expected an '=' sign defining a property"
    );
}

#[test]
fn every_refusal_message_is_free_of_input_text() {
    let inputs = [
        "SECRET-1 = v\n",
        "[profile SECRET-2]\nSECRET-3 : x\n",
        "[profile SECRET-4]\n= SECRET-5\n",
        "[profile SECRET-6\nk = v\n",
        "[profile SECRET-7]\n  SECRET-8\n",
        "[profile SECRET-9]\ns3 =\n  invalid SECRET-10\n",
        "\u{FEFF}[SECRET-11]\n",
    ];
    for input in inputs {
        let error = config_refusal(input);
        let shown = format!("{error} | {error:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}

#[test]
fn debug_output_of_decoded_values_hides_secrets_and_keeps_names() {
    let set = decode(
        "[profile dev]\nregion = us-east-1\n",
        "[dev]\naws_secret_access_key = SECRET-VALUE-9f3\n",
    );
    let shown = format!("{set:?} {:?}", set.profile("dev").unwrap());
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.contains("dev"), "{shown}");
    assert!(shown.contains("aws_secret_access_key"), "{shown}");
    let services = config_only("[services s]\nd =\n  endpoint_url = http://SECRET-HOST\n");
    assert!(!format!("{services:?} {:?}", services.services.get("s").unwrap()).contains("SECRET"));
}
