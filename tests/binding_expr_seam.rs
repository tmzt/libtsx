//! **The expression seam, from outside the crate.**
//!
//! `impl TryFrom<&str> for BindingExpr` and `impl From<&BindingExpr> for
//! String` are the two rungs across the TEXT boundary for ONE expression
//! subtree - no document anywhere. This file exercises them through the public
//! API only, which is the whole claim: a caller that holds a fragment of
//! TypeScript can get a node, and a caller that holds a node can get the
//! fragment back.
//!
//! The law is that the two are **inverse over the image of the parse**.
//! `libhbui`'s `codec_round_trip.rs` measures it over every authored `.tsx` in
//! the repository and over an exhaustive operand/position matrix; this file
//! cannot see that corpus (libtsx depends on no peer crate, by design), so it
//! carries the shapes that decide the SEAM rather than the shapes that decide
//! the lowering: the wrapper's effect on a record, the refusal kinds, and one
//! representative of each variant.
//!
//! **Gated on `parse`**, like `parser_host.rs`: half the seam is the parser.

#![cfg(feature = "parse")]

use libtsx::ParseError;
use libtsx::dag::{BindingExpr, BindingLiteral};

/// Text -> node -> text, which is the direction an author's edit travels.
fn round_trips(source: &str) -> BindingExpr {
    let expr = BindingExpr::try_from(source)
        .unwrap_or_else(|e| panic!("{source} did not lower: {e}"));
    assert_eq!(
        String::from(&expr),
        source,
        "the emitted text is not the source it came from"
    );
    let again = BindingExpr::try_from(String::from(&expr).as_str())
        .unwrap_or_else(|e| panic!("{source} did not lower a second time: {e}"));
    assert_eq!(again, expr, "the second lowering differs from the first");
    expr
}

#[test]
fn one_expression_of_each_variant_crosses_the_seam_and_comes_back() {
    // Every spelling below is already emit's OWN spelling, which is what makes
    // `emitted == source` the right assertion rather than a formatting
    // coincidence: a seam whose two halves disagreed about spacing would fail
    // here and be a real finding.
    for source in [
        "null",
        "true",
        "7",
        "\"s\"",
        "props.a.b",
        "[1, 2]",
        "{a: 1}",
        "{}",
        "g(1)",
        "ns.g(1)",
        "propsOf<Order>()",
        "h().x",
        "a ?? b",
        "a ?? b ?? c",
        "c ? t : o",
        "a === b",
        "a == b",
        "(x: number) => x",
        "(x: number) => ({a: 1})",
        // F1's shape: the body's leading TOKEN is a brace, so the body is
        // parenthesised even though it is not a record.
        "(x: number) => ({a: 1} ?? z)",
        "async () => {\n    return 1;\n}",
    ] {
        round_trips(source);
    }
}

/// **A string literal comes back as the string it went in as.**
///
/// The emit used to push `"`, the value, `"` (libhbui's `codec_round_trip.rs`,
/// F3): a quote ended the literal early, a newline left it unterminated, and a
/// backslash was eaten by the re-parse as an escape nobody wrote - `back\slash`
/// came back `backslash`, valid text and a different string.
#[test]
fn a_string_literal_is_emitted_escaped() {
    for (source, value) in [
        (r#""quote\"inside""#, "quote\"inside"),
        (r#""line\nbreak""#, "line\nbreak"),
        (r#""back\\slash""#, "back\\slash"),
        (r#""carriage\rreturn""#, "carriage\rreturn"),
        (r#""tab\there""#, "tab\there"),
        // A control character has no printable spelling at all.
        (r#""\u0001""#, "\u{1}"),
    ] {
        let expr = round_trips(source);
        assert_eq!(
            expr,
            BindingExpr::Literal(BindingLiteral::String(value.into())),
            "{source} did not lower to the string it spells"
        );
    }

    // **Not over-escaped**, which is the failure a naive fix would be: an
    // apostrophe, a dollar, a backtick and a printable non-ASCII character are
    // not escapes, and a string full of them is written the way it was read.
    // (The `\u{e9}` is spelled as an escape HERE so this file stays ASCII; the
    // string it denotes is one ordinary character.)
    for source in [
        r#""plain text 1.0""#,
        r#""it's $5 (100%) - `ok`""#,
        "\"caf\u{e9}\"",
    ] {
        round_trips(source);
    }
}

#[test]
fn a_record_crosses_the_seam_although_a_bare_brace_opens_a_block() {
    // The one shape that decides whether the seam needs its wrapping
    // parentheses. `{a: 1};` as a STATEMENT is a labelled block, so a seam that
    // parsed the fragment bare would refuse the expression the emitter most
    // needs to put back - and `From`/`TryFrom` would not be inverse.
    assert_eq!(
        BindingExpr::try_from("{a: 1}"),
        Ok(BindingExpr::Record(vec![(
            "a".into(),
            BindingExpr::Literal(BindingLiteral::Number(1.0))
        )]))
    );
    // The known-bad twin: the wrapper is not a licence to accept a STATEMENT.
    // `{const q = 1;}` is a block whichever way it is read, and a seam that
    // silently made it an empty record would pass every test above.
    assert!(BindingExpr::try_from("{const q = 1;}").is_err());
}

/// **A key no identifier can spell keeps its quotes.**
///
/// The parse takes a `PropertyKey::StringLiteral` as readily as a
/// `StaticIdentifier`, so `{"quoted-key": 1}` is an authorable record - and the
/// emit wrote every key bare, which made `{quoted-key: 1}`, a syntax error
/// (libhbui's `codec_round_trip.rs`, F4).
#[test]
fn a_record_key_that_is_not_an_identifier_is_quoted() {
    for source in [
        r#"{"quoted-key": 1}"#,
        r#"{"with space": 1}"#,
        r#"{"0leading": 1}"#,
        r#"{"": 1}"#,
        r#"{a: 1, "b-c": 2}"#,
    ] {
        round_trips(source);
    }
    // The boundary, both ways it can be written: an identifier key is emitted
    // BARE, so a quoted one that did not need the quotes comes back without
    // them - one spelling for one key, which is what stops a publish
    // respelling every record in the corpus.
    assert_eq!(
        String::from(&BindingExpr::try_from(r#"{"a": 1}"#).expect("lowers")),
        "{a: 1}"
    );
    round_trips("{a: 1}");
}

/// **A redundant parenthesis does not make a second shape.**
///
/// `(a).b` is `a.b`, and the lowering says so: a chain whose base lowers to a
/// NAME is one `Path`, whatever parentheses the author left in it. The defect
/// this pins down (`libhbui`'s `codec_round_trip.rs`, F2) was that the
/// parenthesised spelling peeled to `Member { base: Path(["a"]), path: ["b"] }`
/// while the bare one lowered to `Path(["a", "b"])` - one source with two dag
/// shapes, and only the second of them recoverable from the emitted text.
#[test]
fn a_parenthesised_member_chain_is_one_name() {
    for (source, segments) in [
        ("(a).b", &["a", "b"][..]),
        ("(props.a).b", &["props", "a", "b"][..]),
        ("(props.a.b).m", &["props", "a", "b", "m"][..]),
        ("(props.a.b).m.n", &["props", "a", "b", "m", "n"][..]),
        // Nested parentheses, and a parenthesised INNER chain: the peel loop
        // walks through both, so neither is a third shape.
        ("((props.a)).b", &["props", "a", "b"][..]),
        ("((props.a).b).c", &["props", "a", "b", "c"][..]),
    ] {
        let expr = BindingExpr::try_from(source).unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(
            expr,
            BindingExpr::Path(segments.iter().map(|s| s.to_string()).collect()),
            "{source} did not lower to one path"
        );
        // And the text comes back WITHOUT the redundant parenthesis, which is
        // the half that makes the round trip close.
        assert_eq!(String::from(&expr), segments.join("."));
    }

    // The boundary, and the over-correction this would be if it went further: a
    // base that is not a name stays a `Member` and keeps the parentheses emit
    // gives it. `design().isAuthoring` is the shape the variant exists for.
    // (Each source below is emit's OWN spelling, so `round_trips` can assert
    // the text came back unchanged: a non-primary base keeps the parentheses
    // `emit_member_base` gives it, which is why the array one is written with
    // them.)
    for source in ["h().x", "design().isAuthoring", "(a ?? b).c", "([1, 2]).length"] {
        let expr = round_trips(source);
        assert!(
            matches!(expr, BindingExpr::Member { .. }),
            "{source} stopped being a member chain: {expr:?}"
        );
    }
}

/// **An inline record type argument keeps its fields.**
///
/// `emit_type_shape` used to write `{}` for a `TypeShape::Record` and say so in
/// a comment - a declared gap, and still a hole in the round trip: the text
/// parsed, so the fields went missing with nothing to report (libhbui's
/// `codec_round_trip.rs`, F5).
#[test]
fn an_inline_record_type_argument_keeps_its_fields() {
    for source in [
        "g<{a: number}>()",
        "g<{a: number, b: string}>()",
        "g<{a?: number}>()",
        "g<{a: {b: boolean}}>()",
        "g<Array<{a: number}>>()",
        r#"g<{"a-b": number}>()"#,
        // The empty record still writes the two braces it always did - and now
        // means them.
        "g<{}>()",
    ] {
        round_trips(source);
    }
}

#[test]
fn a_refusal_says_which_kind_it_was() {
    // Not TypeScript.
    assert!(matches!(
        BindingExpr::try_from("a ??"),
        Err(ParseError::Syntax(_))
    ));
    // TypeScript the owned vocabulary has no shape for - these parse, and are
    // declined one level up. Both kinds are refusals; conflating them would
    // tell a caller to fix the wrong thing.
    for source in ["`a ${b}`", "{...rest}", "a + b", "xs[0]", "a?.b"] {
        assert!(
            matches!(BindingExpr::try_from(source), Err(ParseError::Binding(_))),
            "{source} was not refused as a binding: {:?}",
            BindingExpr::try_from(source)
        );
    }
    // Two expressions are not one expression.
    assert!(BindingExpr::try_from("a; b").is_err());
    // And every refusal renders ASCII - these strings reach the editor's
    // live-parse status strip.
    for source in ["a ??", "`a ${b}`", "a; b"] {
        let message = BindingExpr::try_from(source).unwrap_err().to_string();
        assert!(
            message.is_ascii(),
            "{source} was refused with non-ASCII text: {message}"
        );
    }
}

#[test]
fn a_fragment_ending_in_a_line_comment_does_not_swallow_the_wrapper() {
    // The seam parses `(<text>\n);`, and the newline is what that trailing
    // `\n` is for. Without it, `//` runs to end of line and comments out the
    // closing parenthesis - the same hazard `emit_comment` handles for a JSX
    // child, arriving at the other end of the crate.
    assert_eq!(
        BindingExpr::try_from("a ?? b // the fallback"),
        Ok(BindingExpr::Coalesce(vec![
            BindingExpr::Path(vec!["a".into()]),
            BindingExpr::Path(vec!["b".into()]),
        ]))
    );
}

#[test]
fn the_seam_lowers_what_an_attribute_lowers() {
    // The seam is a door onto the SAME lowering the document path uses, and
    // this is what says so from outside: the node an attribute carries and the
    // node the seam returns are one value.
    for source in [
        "props.label ?? \"none\"",
        "c ? t : o",
        "(x: number) => ({a: 1} ?? z)",
        "design().isAuthoring",
    ] {
        let document = libtsx::parse_tsx(&format!("<Probe v={{{source}}} />\n"))
            .unwrap_or_else(|e| panic!("{source}: {e:?}"));
        let libtsx::Node::Element(element) = &document.root_nodes[0] else {
            panic!("{source}: no root element");
        };
        let Some((_, libtsx::AttrValue::BindingExpr(via_attribute))) = element.attrs.first() else {
            panic!("{source}: the probe attribute is not a binding expression");
        };
        assert_eq!(
            &BindingExpr::try_from(source).expect("lowers"),
            via_attribute,
            "{source} lowered differently through the two doors"
        );
    }
}
