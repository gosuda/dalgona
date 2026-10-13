use std::path::{Path, PathBuf};

use super::json::{JsonReader, Select};
use super::{
    ArgReader, EditStyle, MAX_FRAMES, MAX_KEY_BYTES, MAX_PATH_BYTES, ReaderEvent, reader_for,
};
use crate::ttsr::scope::{ScopeInput, ScopeValue, admits_tool, parse_scope};
use crate::ttsr::value::Origin;

use ReaderEvent::{Added, ItemEnd, ItemStart, Path as PathEv};

fn added(text: &str) -> ReaderEvent {
    Added(text.to_owned())
}

fn path(text: &str) -> ReaderEvent {
    PathEv(text.to_owned())
}

/// Coalesces adjacent `Added` events so chunkings compare equal.
fn merged(events: Vec<ReaderEvent>) -> Vec<ReaderEvent> {
    let mut out: Vec<ReaderEvent> = Vec::new();
    for event in events {
        if let (Some(Added(last)), Added(text)) = (out.last_mut(), &event) {
            last.push_str(text);
            continue;
        }
        if matches!(&event, Added(text) if text.is_empty()) {
            continue;
        }
        out.push(event);
    }
    out
}

fn run(mut reader: Box<dyn ArgReader>, chunks: &[&str]) -> Vec<ReaderEvent> {
    let mut out = Vec::new();
    for chunk in chunks {
        reader.feed(chunk, &mut out);
    }
    reader.close(&mut out);
    merged(out)
}

/// Feeds `input` whole, split at every char boundary, and one char at a
/// time; every run must produce `expected`.
fn assert_chunked(make: &dyn Fn() -> Box<dyn ArgReader>, input: &str, expected: &[ReaderEvent]) {
    assert_eq!(run(make(), &[input]), expected, "whole input");
    for (at, _) in input.char_indices().skip(1) {
        let got = run(make(), &[&input[..at], &input[at..]]);
        assert_eq!(got, expected, "split at byte {at}");
    }
    let chars: Vec<String> = input.chars().map(String::from).collect();
    let chunks: Vec<&str> = chars.iter().map(String::as_str).collect();
    assert_eq!(run(make(), &chunks), expected, "one char per delta");
}

fn tool(name: &'static str, style: EditStyle) -> impl Fn() -> Box<dyn ArgReader> {
    move || reader_for(name, style)
}

/// A key of exactly [`MAX_KEY_BYTES`] bytes.
const EDGE_KEY: &str = concat!(
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
);
/// A key one byte over [`MAX_KEY_BYTES`].
const LONG_KEY: &str = concat!(
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
    "k",
);

#[test]
fn reader_escape_splits() {
    let input = r#"{"command":"sl\u0065ep 5"}"#;
    let expected = [ItemStart, added("sleep 5"), ItemEnd];
    assert_chunked(&tool("exec", EditStyle::Anchor), input, &expected);

    let pair = r#"{"command":"a\ud83d\ude00b\\\"\n\t\/"}"#;
    let expected = [ItemStart, added("a\u{1F600}b\\\"\n\t/"), ItemEnd];
    assert_chunked(&tool("exec", EditStyle::Anchor), pair, &expected);
}

/// Verifies that path-restricted matches wait for their item path and
/// that an unresolved match is discarded when the item ends.
fn gated_fire(events: &[ReaderEvent], scope_text: &str) -> Option<usize> {
    let origin = Origin::User(PathBuf::from("/rules/no-bad.md"));
    let (scope, _) = parse_scope(ScopeInput {
        origin: &origin,
        value: ScopeValue::String(scope_text),
        known_tools: None,
        extra_tokens: &[],
    });
    let root = Path::new("/ws");
    let mut items: Vec<(Option<String>, String, bool)> = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if items.is_empty() && matches!(event, Added(_) | PathEv(_)) {
            items.push((None, String::new(), false));
        }
        match event {
            ItemStart => items.push((None, String::new(), false)),
            ItemEnd => {
                items.pop();
            }
            PathEv(p) => {
                let item = items.last_mut().unwrap();
                item.0 = Some(p.clone());
                if item.2 && admits_tool(&scope, "patch", Some(p), root) {
                    return Some(index);
                }
                item.2 = false;
            }
            Added(text) => {
                let item = items.last_mut().unwrap();
                item.1.push_str(text);
                if item.1.contains("bad(") {
                    match &item.0 {
                        None => item.2 = true,
                        Some(p) if admits_tool(&scope, "patch", Some(p), root) => {
                            return Some(index);
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
    None
}

#[test]
fn patch_gating() {
    let make = tool("patch", EditStyle::Replace);
    let ml = r#"{"changes":[{"new":"bad()","path":"a.ml"}]}"#;
    let expected = [
        ItemStart,
        ItemStart,
        added("bad()"),
        path("a.ml"),
        ItemEnd,
        ItemEnd,
    ];
    assert_chunked(&make, ml, &expected);
    assert_eq!(gated_fire(&expected, "tool:patch(*.ml)"), Some(3));

    let txt = r#"{"changes":[{"new":"bad()","path":"a.txt"}]}"#;
    let events = run(make(), &[txt]);
    assert!(events.contains(&path("a.txt")));
    assert_eq!(gated_fire(&events, "tool:patch(*.ml)"), None);

    let no_path = r#"{"changes":[{"new":"bad()"},{"path":"a.ml","new":"ok"}]}"#;
    let events = run(make(), &[no_path]);
    assert_eq!(gated_fire(&events, "tool:patch(*.ml)"), None);
}

#[test]
fn replace_style_emits_only_new_and_create() {
    let make = tool("patch", EditStyle::Replace);
    let input =
        r#"{"changes":[{"path":"a","old":"gone","new":"x","create":"y"},{"path":"b","new":""}]}"#;
    let expected = [
        ItemStart,
        ItemStart,
        path("a"),
        added("x\ny"),
        ItemEnd,
        ItemStart,
        path("b"),
        ItemEnd,
        ItemEnd,
    ];
    assert_chunked(&make, input, &expected);
}

#[test]
fn every_string_covers_arrays_numbers_and_literals() {
    let make = tool("search", EditStyle::Replace);
    let input = r#" {"a":[1,-2.5e+3,true,null,"p"],"b":{"c":"q"},"d":false,"e":0} "#;
    let expected = [
        ItemStart,
        added("p"),
        ItemStart,
        added("q"),
        ItemEnd,
        ItemEnd,
    ];
    assert_chunked(&make, input, &expected);
}

#[test]
fn reader_limits() {
    assert_eq!(EDGE_KEY.len(), MAX_KEY_BYTES);
    assert_eq!(LONG_KEY.len(), MAX_KEY_BYTES + 1);
    let input = format!(r#"{{"{LONG_KEY}":"hit"}}"#);
    let reader = Box::new(JsonReader::new(Select::Members(&[LONG_KEY])));
    assert_eq!(run(reader, &[&input]), [ItemStart, ItemEnd]);
    let input = format!(r#"{{"{EDGE_KEY}":"hit"}}"#);
    let reader = Box::new(JsonReader::new(Select::Members(&[EDGE_KEY])));
    assert_eq!(run(reader, &[&input]), [ItemStart, added("hit"), ItemEnd]);

    let exec = tool("exec", EditStyle::Replace);
    let long = "a".repeat(MAX_PATH_BYTES + 1);
    let events = run(exec(), &[&format!(r#"{{"path":"{long}"}}"#)]);
    assert_eq!(events, [ItemStart, added(&long), ItemEnd]);
    let edge = "a".repeat(MAX_PATH_BYTES);
    let events = run(exec(), &[&format!(r#"{{"path":"{edge}"}}"#)]);
    assert_eq!(events, [ItemStart, added(&edge), path(&edge), ItemEnd]);

    let deep_ok = format!("{}\"x\"{}", "[".repeat(MAX_FRAMES), "]".repeat(MAX_FRAMES));
    assert_eq!(run(exec(), &[&deep_ok]), [added("x")]);
    let deep = format!("{}\"x\"", r#"{"a":"#.repeat(MAX_FRAMES + 1));
    let mut reader = exec();
    let mut out = Vec::new();
    reader.feed(&deep, &mut out);
    assert_eq!(
        out,
        vec![ItemStart; MAX_FRAMES],
        "deep nesting emits nothing more"
    );
    out.clear();
    reader.close(&mut out);
    assert_eq!(out, vec![ItemEnd; MAX_FRAMES]);

    let malformed = r#"{"a":tru "b":"secret"}"#;
    let mut reader = exec();
    let mut out = Vec::new();
    reader.feed(malformed, &mut out);
    reader.feed(r#""more"}"#, &mut out);
    assert_eq!(out, [ItemStart]);
    reader.close(&mut out);
    assert_eq!(out, [ItemStart, ItemEnd]);
    assert_eq!(
        run(exec(), &[r#"{"a":"x"} "y""#]),
        [ItemStart, added("x"), ItemEnd]
    );
    assert_eq!(
        run(exec(), &["{\"a\":\"x\ny\"}"]),
        [ItemStart, added("x"), ItemEnd]
    );

    let surrogate = r#"{"c":"a\ud800b\udc00c\ud800\ud800\udc00d\ud800"}"#;
    let expected = [
        ItemStart,
        added("a\u{FFFD}b\u{FFFD}c\u{FFFD}\u{10000}d\u{FFFD}"),
        ItemEnd,
    ];
    assert_chunked(&exec, surrogate, &expected);
    let cut_high = r#"{"c":"z\ud800"#;
    assert_chunked(&exec, cut_high, &[ItemStart, added("z\u{FFFD}"), ItemEnd]);

    let cut = r#"{"changes":[{"path":"a.ml","new":"bad"#;
    let expected = [
        ItemStart,
        ItemStart,
        path("a.ml"),
        added("bad"),
        ItemEnd,
        ItemEnd,
    ];
    assert_chunked(&tool("patch", EditStyle::Replace), cut, &expected);
}

#[test]
fn hashline_rows_freeform_and_json() {
    let payload = "*** Begin Patch\n[src/a.ml#A1B2]\nPUT 1.=2:\n+one\n+\n+two\nCUT 3\n+not added\nPUT x:\n+bad locator\n[b.rs#NEW]\nPUT >$:\n+three\r\n*** End Patch\n";
    let expected = [
        ItemStart,
        path("src/a.ml"),
        added("one\n\ntwo"),
        ItemEnd,
        ItemStart,
        path("b.rs"),
        added("three"),
        ItemEnd,
    ];
    let make = tool("patch", EditStyle::Hashline);
    assert_chunked(&make, payload, &expected);
    let json = format!(r#"{{"input":{}}}"#, json_string(payload));
    assert_chunked(&make, &json, &expected);

    let bad_header = "[a#abcd]\nPUT 1:\n+x\n[c#ABCD] \nPUT 2*:\n+y\n";
    assert_chunked(&make, bad_header, &[added("x\ny")]);

    let cut = "[a.ml#NEW]\nPUT >$:\n+partial";
    assert_chunked(
        &make,
        cut,
        &[ItemStart, path("a.ml"), added("partial"), ItemEnd],
    );
}

#[test]
fn apply_patch_rows() {
    let payload = "*** Begin Patch\n*** Add File: path/add.py\n+abc\n+def\n*** Delete File: gone.py\n+not added\n*** Update File: u.py \n*** Move to: v.py\n@@ def f():\n-    pass\n+    return 123\n+++\n context\r\n*** End of File\n*** End Patch\n+after end\n";
    let expected = [
        ItemStart,
        path("path/add.py"),
        added("abc\ndef"),
        ItemEnd,
        ItemStart,
        path("u.py"),
        added("    return 123\n++"),
        ItemEnd,
    ];
    let make = tool("patch", EditStyle::ApplyPatch);
    assert_chunked(&make, payload, &expected);
    let json = format!(r#"{{"input":{}}}"#, json_string(payload));
    assert_chunked(&make, &json, &expected);

    let long = format!("*** Add File: {}\n+x\n", "p".repeat(MAX_PATH_BYTES + 1));
    assert_eq!(run(make(), &[&long]), [ItemStart, added("x"), ItemEnd]);
}

#[test]
fn anchor_rows_skip_find_text() {
    let payload = "```\n*** File: src/a.rs #0123ABCD\n*** Find @3\nold text\n*** Replace\nnew one\n\n*** Findings stay\n*** File: b.rs\nx\n*** Find all\nold\n*** Insert After\ntail\n*** Delete File: c.rs\n*** New File:  d.md \n# D\n*** Move: e -> f\nno\n```\n";
    let expected = [
        ItemStart,
        path("src/a.rs"),
        added("new one\n\n*** Findings stay"),
        ItemEnd,
        ItemStart,
        path("b.rs"),
        added("tail"),
        ItemEnd,
        ItemStart,
        path("d.md"),
        added("# D"),
        ItemEnd,
    ];
    let make = tool("patch", EditStyle::Anchor);
    assert_chunked(&make, payload, &expected);

    let second = "*** File: a.rs\n*** Find\nx\n*** Replace File\n```\nbody\n*** File:\n*** Find\ny\n*** Insert Before\nz\n";
    let expected = [ItemStart, path("a.rs"), added("```\nbody\nz"), ItemEnd];
    assert_chunked(&make, second, &expected);
}

/// Encodes `text` as a JSON string literal with escapes only.
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            'e' => out.push_str("\\u0065"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
