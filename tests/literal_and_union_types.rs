//! **`TypeShape` can spell a literal type and a union** - the two most
//! ordinary TypeScript type forms, which this vocabulary could not hold.
//!
//! MEASURED before the two variants existed, and this file is what those
//! measurements became:
//!
//! ```text
//! interface ButtonProps { tone?: "primary" | "danger" }
//!   -> Err("`tone`: a union of 2 types is not modelled - the semantic AST has
//!           no sum type, only `T | undefined` / `T | null` (which is an Option)")
//! interface Row { kind: "handle" }
//!   -> Named("unknown")
//! ```
//!
//! The first is an outright refusal of source an author writes every day. The
//! second is the worse half, because it PARSES: `"handle"` fell to
//! `type_shape`'s `_ =>` catch-all and became the same `Named("unknown")` an
//! unmodelled `symbol` or mapped type becomes, so a discriminant a record keys
//! on arrived downstream indistinguishable from a typo.
//!
//! **Out here rather than in `parse.rs`'s inline module** for the reason
//! `parser_host.rs` states: these go through the public API only, so they
//! cannot reach a private helper and cannot pass for a reason a consumer would
//! not get.

#![cfg(feature = "parse")]

use libtsx::dag::{LiteralValue, TypeShape};
use libtsx::extract_interfaces;

/// `tone?: "primary" | "danger"` - the exact declaration the refusal blocked.
///
/// **The whole declaration survives**, which is the half that would be easy to
/// lose: the point is not that a union appears somewhere, it is that an
/// interface carrying one still comes back with its name, its other fields and
/// this field's optionality intact.
#[test]
fn a_string_literal_union_parses() {
    let interfaces = extract_interfaces(
        r#"
            interface ButtonProps {
                label: string;
                tone?: "primary" | "danger";
                onTap: string;
            }
        "#,
    )
    .expect("a two-member string-literal union parses");

    assert_eq!(interfaces.len(), 1);
    let props = &interfaces[0];
    assert_eq!(props.name, "ButtonProps");
    assert_eq!(
        props.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["label", "tone", "onTap"],
        "the union field does not consume its neighbours",
    );

    let tone = &props.fields[1];
    assert!(tone.optional, "`tone?` is still optional");
    assert_eq!(
        tone.ty,
        TypeShape::Union(vec![
            TypeShape::Literal(LiteralValue::String("primary".into())),
            TypeShape::Literal(LiteralValue::String("danger".into())),
        ]),
    );
    assert_eq!(props.fields[0].ty, TypeShape::String);
}

/// A literal in type position is the literal, **not** `Named("unknown")`.
///
/// All four literal forms TypeScript has a carrier for, because "string at
/// minimum" would have left `1` and `true` degrading through the same
/// catch-all the string just left - and a vocabulary that spells one literal
/// and silently loses the next three is harder to reason about than one that
/// spells none.
#[test]
fn a_literal_type_is_not_unknown() {
    let interfaces = extract_interfaces(
        r#"
            interface Row {
                kind: "handle";
                count: 42;
                ratio: 1.5;
                step: -1;
                open: true;
            }
        "#,
    )
    .expect("literals in type position parse");

    let shapes: Vec<&TypeShape> = interfaces[0].fields.iter().map(|f| &f.ty).collect();
    assert_eq!(
        shapes,
        [
            &TypeShape::Literal(LiteralValue::String("handle".into())),
            &TypeShape::Literal(LiteralValue::Int64(42)),
            &TypeShape::Literal(LiteralValue::Float64(1.5)),
            &TypeShape::Literal(LiteralValue::Int64(-1)),
            &TypeShape::Literal(LiteralValue::Bool(true)),
        ],
    );
    // The claim in the negative, stated as the defect it replaces: not one of
    // them is the catch-all's answer.
    assert!(
        !shapes.contains(&&TypeShape::Named("unknown".to_string())),
        "{shapes:?}",
    );
}

/// **An integer literal type keeps its exact value past 2^53**, because
/// `literal_shape` reuses `numeric_literal` rather than reading oxc's already
/// rounded `f64`. Pinned here because a second reading in type position is
/// exactly the kind of quiet divergence that would never be noticed: the
/// declaration would still parse, and the number would just be a nearby number.
#[test]
fn a_wide_integer_literal_type_is_not_rounded() {
    let interfaces =
        extract_interfaces("interface P { id: 4605617453661332513; }").expect("parses");
    assert_eq!(
        interfaces[0].fields[0].ty,
        TypeShape::Literal(LiteralValue::Int64(4_605_617_453_661_332_513)),
        "the f64 route would have given 4605617453661332480",
    );
}

/// **The emit is the inverse of the parse on both new variants** - the law the
/// crate header states for this seam, measured for the shapes this step adds.
///
/// The union inside each container is deliberate: `|` binds looser than every
/// other spelling, so a container that does not bracket its contents would
/// re-parse as something else entirely. `(A | B)["k"]` is the one that
/// actually bites - written bare it is `A | (B["k"])`, a different type that
/// parses cleanly.
#[test]
fn the_emit_re_parses_to_the_same_shape() {
    for src in [
        r#"interface P { a: "primary" | "danger"; }"#,
        r#"interface P { a: 1 | -1 | 0; }"#,
        r#"interface P { a: true | false; }"#,
        r#"interface P { a: "x"; }"#,
        r#"interface P { a: Id | Blank | undefined; }"#,
        r#"interface P { a: Array<"x" | "y">; }"#,
        r#"interface P { a: {b: "x" | "y"}; }"#,
        r#"interface P { a: Foo<"x" | "y">; }"#,
        r#"interface P { a: (Id | Blank)["k"]; }"#,
    ] {
        let parsed = extract_interfaces(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
        let text = String::from(&parsed[0].fields[0].ty);
        let round = extract_interfaces(&format!("interface P {{ a: {text}; }}"))
            .unwrap_or_else(|e| panic!("{src} emitted `{text}`: {e:?}"));
        assert_eq!(
            parsed[0].fields[0].ty, round[0].fields[0].ty,
            "{src} emitted `{text}`, which re-parsed as something else",
        );
    }
}

/// **THE TAIL OF THE ENUM, PINNED AT THE BYTE - AND RE-PINNED, ONCE, WHEN THE
/// THREE KEY OPERATORS LEFT IT.**
///
/// `TypeShape` is persisted positionally - postcard writes a bare varint
/// variant index with no name beside it - so a variant INSERTED where it reads
/// better renumbers every variant after it, and every already-committed
/// `.hbdef` decodes as a different declaration with no error anywhere. That
/// rule is why `Literal` and `Union` were appended rather than filed beside
/// `String` and `Option`, and this test is what makes the placement checkable.
///
/// # Why these numbers MOVED, and why that is a bump rather than a defect
///
/// It read `the_new_variants_are_appended_at_17_and_18` and pinned
/// `Omit` 13, `Pick` 14, `Extends` 15, `IndexedAccess` 16, `Literal` 17,
/// `Union` 18. `Omit`, `Pick` and `IndexedAccess` are REGISTERED TYPE FUNCTIONS
/// now (`libhbdata::typeexpr::REGISTRY`, FACT_IMPLEMENTATION A3): the operators
/// are `TypeShape::Apply` applications carrying a name, exactly as `Partial`
/// always was, so their variants were removed and everything after them shifted
/// down by three.
///
/// **A removal cannot be append-managed**, which is the same sentence
/// `libhbui::codec::HBDEF_VERSION` records for each of its own bumps: there are
/// no names in the bytes, so an old record's `15` is read as `Extends` by the
/// writer and as `Union` by this build. That is exactly what the version header
/// refuses, and it is refused rather than migrated - `HBDEF_VERSION` and
/// `NODEGRAPH_VERSION` both moved in the commit that removed these variants,
/// and every committed artifact was regenerated.
///
/// So the number to read here is `Extends`: it is the FIRST variant after the
/// three that left, and 13 is 15 minus 3 exactly. If it were anything else, the
/// removal took something else with it.
#[test]
fn the_tail_of_the_enum_is_pinned_where_the_registry_left_it() {
    /// postcard writes the discriminant as a leading varint; every index here
    /// is under 128, so it is the first byte.
    fn index(shape: &TypeShape) -> u8 {
        postcard::to_allocvec(shape).expect("encode")[0]
    }

    // Everything up to and including the unsigned pair is where it always was:
    // the removal was of three CONSECUTIVE variants after them, so nothing here
    // may move.
    assert_eq!(index(&TypeShape::Bool), 0);
    assert_eq!(index(&TypeShape::Apply { constructor: "F".into(), args: vec![] }), 10);
    assert_eq!(index(&TypeShape::U32), 11);
    assert_eq!(index(&TypeShape::U64), 12);

    // `Omit` (13), `Pick` (14) and `IndexedAccess` (16) were here. Each is an
    // `Apply` now and therefore index 10, which the line above already pins -
    // there is no fourth index to assert, and that IS the saving the registry
    // makes: a registered function costs no variant.
    assert_eq!(
        index(&TypeShape::Extends { base: Box::new(TypeShape::Bool) }),
        13,
        "the first variant after the three that left - 15 minus 3, exactly",
    );
    assert_eq!(index(&TypeShape::Literal(LiteralValue::String("x".into()))), 14);
    assert_eq!(index(&TypeShape::Union(vec![])), 15);

    // Round trip, nested, with every literal width in one value - so a payload
    // that encodes but does not decode cannot pass on the index check alone.
    let shape = TypeShape::Option(Box::new(TypeShape::List(Box::new(TypeShape::Union(vec![
        TypeShape::Literal(LiteralValue::String("primary".into())),
        TypeShape::Literal(LiteralValue::Bool(false)),
        TypeShape::Literal(LiteralValue::Int32(-7)),
        TypeShape::Literal(LiteralValue::Int64(4_605_617_453_661_332_513)),
        TypeShape::Literal(LiteralValue::Float32(0.5)),
        TypeShape::Literal(LiteralValue::Float64(1.5)),
        TypeShape::Named("Blank".into()),
    ])))));
    let bytes = postcard::to_allocvec(&shape).expect("encode");
    let back: TypeShape = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back, shape);
}
