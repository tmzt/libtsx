//! **libtsx against a mock [`ParserHost`]** - Rule 52's executable test.
//!
//! This file is the *second embedding*. It reaches libtsx through the public
//! API only, hands the parse a provider of its own, and grants a vocabulary
//! **no real embedding has**: `host:zork`, `frobnicate`, `onGrommet`. If these
//! tests passed only against the names libhbui uses they could not detect a
//! name leaking down into the parser - they would *be* the leak.
//!
//! So the deliberately odd names are the mechanism, not decoration:
//!
//! * nothing here is called `navigate`, `onTap` or `host:effects`;
//! * nothing here depends on libhbui - not as a dev-dependency, not for a
//!   fixture. [`Zork`] supplies everything the parser asks for.
//!
//! It also carries the effect-binding tests themselves (Rules 46a, 48). They
//! live here rather than in `parse.rs`'s inline module for the same reason: an
//! inline test can reach a private helper, and the point of these is that an
//! embedding's whole surface is the public one.
//!
//! Every source below goes through the real parser. Several are hand-written
//! malformations, which is Rule 43's exception - **the point IS the shape**:
//! what is being ruled out is source an authoring surface could produce, so
//! there is nothing else to parse them from.

use libtsx::dag::{
    AttrValue, EffectError, Expr, FieldDecl, FuncSig, ImportKind, NamedEffect, Node, ParserHost,
    Resolution, TsxDocument, TypeShape,
};
use libtsx::{ParseCtx, ParseError, parse_tsx};

// --- the mock (LIBHBUI_PLAN Rule 52) ------------------------------------------

/// The host namespace [`Zork`] grants. A placeholder by construction: no
/// embedding of libtsx grants it, so a parser that only works against a real
/// embedding's names fails here.
const ZORK: &str = "host:zork";

/// A second granted namespace, so "the grant" is never one entry with one
/// answer.
const GRUE: &str = "host:grue";

/// A **Script**: compiled elsewhere, named by its display name, and unable to
/// supply an effect however it is called (Rule 48).
const PLOVER: &str = "Plover Provider";

/// A **package path**: resolved outside the parse entirely.
const PLUGH: &str = "@xyzzy/plugh";

/// The mock provider: three specifiers, one of each answer [`Resolution`] has.
///
/// It owns its signatures so [`ParserHost::resolve`] can hand out borrowed
/// slices, which is the shape a real embedding has too - a grant is data the
/// embedding holds, not something the parse allocates.
struct Zork {
    zork: Vec<FuncSig>,
    grue: Vec<FuncSig>,
}

impl Zork {
    fn new() -> Self {
        Self {
            zork: vec![sig(
                "frobnicate",
                vec![param("sprocket", TypeShape::String)],
            )],
            grue: vec![sig(
                "wibble",
                vec![
                    param("after", TypeShape::S32),
                    param("loudly", TypeShape::Bool),
                ],
            )],
        }
    }
}

impl ParserHost for Zork {
    fn resolve(&self, specifier: &str) -> Option<Resolution<'_>> {
        match specifier {
            ZORK => Some(Resolution::Host(&self.zork)),
            GRUE => Some(Resolution::Host(&self.grue)),
            PLOVER => Some(Resolution::Script),
            PLUGH => Some(Resolution::Package),
            _ => None,
        }
    }
}

fn sig(name: &str, params: Vec<FieldDecl>) -> FuncSig {
    FuncSig {
        name: name.into(),
        params,
        result: None,
    }
}

fn param(name: &str, ty: TypeShape) -> FieldDecl {
    FieldDecl {
        name: name.into(),
        ty,
        optional: false,
    }
}

/// The context the mock is offered through - one value, built the one way
/// there is (Rule 49).
fn ctx() -> ParseCtx {
    ParseCtx::builder().set_host(Zork::new()).build()
}

/// The one event-binding attribute of a source with exactly one element.
fn only_effect(src: &str) -> AttrValue {
    let doc = ctx().parse_tsx(src).expect("parses");
    let Node::Element(el) = &doc.root_nodes[0] else {
        panic!("root is an element")
    };
    el.attrs
        .iter()
        .find(|(k, _)| k.starts_with("on"))
        .map(|(_, v)| v.clone())
        .expect("the element declares an event binding")
}

/// What a source is refused with.
fn refusal(src: &str) -> EffectError {
    match ctx().parse_tsx(src) {
        Err(ParseError::Effect(e)) => e,
        Err(ParseError::Syntax(d)) => panic!("the source does not even parse: {d:?}"),
        Err(other) => panic!("refused, and not for an effect: {other}"),
        Ok(doc) => panic!("accepted, and produced {doc:?}"),
    }
}

// --- the provider answers all three (Rule 52) ---------------------------------

/// **One provider, three answers, and the parse does something different with
/// each.**
///
/// This is the whole reason [`ParserHost`] is a package provider rather than an
/// effects grant: a `Host` namespace supplies the signature an effect resolves
/// against, while a Script and a package resolve *fine* and supply nothing -
/// and a call through one of those is refused for a reason that names what it
/// really is. An effects-only interface could not tell the last two apart from
/// a name that was never imported.
#[test]
fn one_provider_answers_script_package_and_host() {
    let host = Zork::new();
    assert!(matches!(host.resolve(ZORK), Some(Resolution::Host(_))));
    assert!(matches!(host.resolve(GRUE), Some(Resolution::Host(_))));
    assert!(matches!(host.resolve(PLOVER), Some(Resolution::Script)));
    assert!(matches!(host.resolve(PLUGH), Some(Resolution::Package)));
    assert_eq!(host.resolve("host:nothing"), None);

    // A granted host import: the call resolves and carries the effect.
    assert_eq!(
        only_effect(&format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            <Widget id="a" onGrommet={{frobnicate("sprocket")}} />
            "#
        )),
        AttrValue::NamedEffect(NamedEffect {
            name: "frobnicate".into(),
            args: vec![Expr::LitStr("sprocket".into())],
        })
    );

    // A Script and a package both import cleanly - resolving them is somebody
    // else's job and always has been...
    for source in [PLOVER, PLUGH] {
        let doc = ctx()
            .parse_tsx(&format!(
                r#"
                import {{ frobnicate }} from "{source}";
                <Widget id="a" />
                "#
            ))
            .expect("an import with a source behind it is not the parser's business");
        assert_eq!(doc.imports[0].source, source);

        // ...and neither can supply an effect. The refusal names the specifier,
        // which is what the provider bought: "you imported that from something
        // compiled" rather than "you never imported that".
        assert_eq!(
            refusal(&format!(
                r#"
                import {{ frobnicate }} from "{source}";
                <Widget id="a" onGrommet={{frobnicate("sprocket")}} />
                "#
            )),
            EffectError::NotAHostImport {
                attr: "onGrommet".into(),
                callee: "frobnicate".into(),
                source: source.into(),
            },
            "a call through a {source} import was not refused as a non-host import",
        );
    }

    // And a name that really was never imported is the other fact, said
    // separately.
    assert_eq!(
        refusal(r#"<Widget id="a" onGrommet={frobnicate("sprocket")} />"#),
        EffectError::Unresolved {
            attr: "onGrommet".into(),
            callee: "frobnicate".into(),
        }
    );
}

/// **A grant no import could name is the embedding's mistake, not the
/// source's.**
///
/// The `host:` scheme is shape rather than a name (Rule 52): it is what lets
/// the parse tell Rule 48's three answers apart *before* consulting the
/// provider. A provider granting under an unschemed specifier has offered a
/// capability nothing can reach, and a silent one is indistinguishable from an
/// effect that does not resolve - so it is refused at the import line.
#[test]
fn a_grant_spelled_without_the_scheme_is_refused() {
    /// A provider with the bug: it grants, and it grants under a name no
    /// `host:` import could ever be recognised as.
    struct Unschemed(Vec<FuncSig>);

    impl ParserHost for Unschemed {
        fn resolve(&self, specifier: &str) -> Option<Resolution<'_>> {
            (specifier == "zork").then_some(Resolution::Host(&self.0))
        }
    }

    let ctx = ParseCtx::builder()
        .set_host(Unschemed(vec![sig(
            "frobnicate",
            vec![param("sprocket", TypeShape::String)],
        )]))
        .build();
    assert_eq!(
        ctx.parse_tsx(
            r#"
            import { frobnicate } from "zork";
            <Widget id="a" onGrommet={frobnicate("sprocket")} />
            "#
        ),
        Err(ParseError::Effect(EffectError::GrantedWithoutScheme {
            source: "zork".into(),
        }))
    );
}

// --- effect bindings (Rules 46a, 48) ------------------------------------------

/// **The whole authoring surface** (Rule 46a): one `on[A-Z]*` attribute whose
/// value is one call.
///
/// The value the attribute carries is the *resolved* host name and its lowered
/// arguments - not the local name, not the source text, and not an `Opaque`.
#[test]
fn an_event_binding_carries_a_named_effect() {
    assert_eq!(
        only_effect(&format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            <Widget id="a" onGrommet={{frobnicate("sprocket")}} />
            "#
        )),
        AttrValue::NamedEffect(NamedEffect {
            name: "frobnicate".into(),
            args: vec![Expr::LitStr("sprocket".into())],
        })
    );

    // An ALIAS is spent at the parse: `fb` never travels. This is what "named"
    // buys - the name in the graph is the host import's, so a reader resolves
    // nothing and two sources that alias differently produce one value.
    assert_eq!(
        only_effect(&format!(
            r#"
            import {{ frobnicate as fb }} from "{ZORK}";
            <Widget id="a" onGrommet={{fb("sprocket")}} />
            "#
        )),
        AttrValue::NamedEffect(NamedEffect {
            name: "frobnicate".into(),
            args: vec![Expr::LitStr("sprocket".into())],
        })
    );

    // The recognition rule is the attribute's NAME, not the tag and not a list:
    // a second effect, on a different attribute, from a second namespace, needs
    // nothing added anywhere. Its arguments lower against the DECLARED types -
    // `250` becomes `LitS32`, not a float.
    assert_eq!(
        only_effect(&format!(
            r#"
            import {{ wibble }} from "{GRUE}";
            <Lamp id="a" onXyzzy={{wibble(250, true)}} />
            "#
        )),
        AttrValue::NamedEffect(NamedEffect {
            name: "wibble".into(),
            args: vec![Expr::LitS32(250), Expr::LitBool(true)],
        })
    );

    // And an ordinary attribute is untouched by any of it.
    let doc = ctx()
        .parse_tsx(&format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            <Widget id="a" height={{56}} onGrommet={{frobnicate("sprocket")}} label="Chat" />
            "#
        ))
        .expect("parses");
    let Node::Element(el) = &doc.root_nodes[0] else {
        panic!()
    };
    assert_eq!(
        el.attrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        vec!["id", "height", "onGrommet", "label"],
        "attributes keep source order",
    );
    assert_eq!(el.attr("height"), Some(&AttrValue::Num(56.0)));
    assert_eq!(el.attr("label"), Some(&AttrValue::Str("Chat".into())));
}

/// **Refusal 1.** An `on..` attribute whose value is not a call.
///
/// Each of these parsed to `AttrValue::Opaque` or `AttrValue::Binding` before
/// the producer existed - an effect erased, and erased *silently*, which is the
/// failure this refusal is for. The withdrawn `onGrommet={namedHandler}`
/// spelling is in the list on purpose: it is the most plausible wrong thing to
/// write.
#[test]
fn an_event_binding_that_is_not_a_call_is_refused() {
    for value in [
        r#"onGrommet={goFrob}"#,                     // a bare identifier
        r#"onGrommet={() => frobnicate("sprocket")}"#, // an arrow function
        r#"onGrommet="sprocket""#,                   // a string
        r#"onGrommet"#,                              // valueless (would be `true`)
        r#"onGrommet={"sprocket"}"#,                 // a string in a container
        r#"onGrommet={props.destination}"#,          // a data binding
        r#"onGrommet={<Widget/>}"#,                  // an element
        r#"onGrommet={frobnicate("sprocket") && x}"#, // a call inside an expression
    ] {
        let src = format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            <Widget id="a" {value} />
            "#
        );
        assert_eq!(
            refusal(&src),
            EffectError::NotACall {
                attr: "onGrommet".into()
            },
            "`{value}` was not refused as a non-call",
        );
    }
}

/// **Refusal 2.** A callee that does not resolve, through the import chain, to
/// a declared host import.
#[test]
fn a_callee_that_resolves_to_no_host_import_is_refused() {
    // Never imported at all.
    assert_eq!(
        refusal(r#"<Widget id="a" onGrommet={frobnicate("sprocket")} />"#),
        EffectError::Unresolved {
            attr: "onGrommet".into(),
            callee: "frobnicate".into(),
        }
    );
    // The LOCAL name is what resolves: an alias means the exported name no
    // longer names anything in this module.
    assert_eq!(
        refusal(&format!(
            r#"
            import {{ frobnicate as fb }} from "{ZORK}";
            <Widget id="a" onGrommet={{frobnicate("sprocket")}} />
            "#
        )),
        EffectError::Unresolved {
            attr: "onGrommet".into(),
            callee: "frobnicate".into(),
        }
    );
    // A member-expression callee with no host import behind it is unresolved
    // rather than accepted - the flat-callee rule holds even where no namespace
    // import is in sight.
    assert_eq!(
        refusal(r#"<Widget id="a" onGrommet={fx.frobnicate("sprocket")} />"#),
        EffectError::Unresolved {
            attr: "onGrommet".into(),
            callee: "fx.frobnicate".into(),
        }
    );
}

/// **Refusal 3.** Argument count and type, against the `FuncSig`.
///
/// This is the check that catches a grant and a call disagreeing by machine
/// instead of by reading.
#[test]
fn arguments_are_checked_against_the_declared_signature() {
    let with = |call: &str| {
        format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            import {{ wibble }} from "{GRUE}";
            <Widget id="a" onGrommet={{{call}}} />
            "#
        )
    };
    assert_eq!(
        refusal(&with("frobnicate()")),
        EffectError::ArgCount {
            attr: "onGrommet".into(),
            effect: "frobnicate".into(),
            declared: 1,
            given: 0,
        }
    );
    assert_eq!(
        refusal(&with(r#"frobnicate("sprocket", "widget")"#)),
        EffectError::ArgCount {
            attr: "onGrommet".into(),
            effect: "frobnicate".into(),
            declared: 1,
            given: 2,
        }
    );
    // A number where a string is declared.
    assert_eq!(
        refusal(&with("frobnicate(3)")),
        EffectError::ArgType {
            attr: "onGrommet".into(),
            effect: "frobnicate".into(),
            index: 0,
            declared: TypeShape::String,
        }
    );
    // And the reverse, on the second parameter, so the index is not always
    // zero.
    assert_eq!(
        refusal(&with(r#"wibble(250, "yes")"#)),
        EffectError::ArgType {
            attr: "onGrommet".into(),
            effect: "wibble".into(),
            index: 1,
            declared: TypeShape::Bool,
        }
    );
    // A fractional literal is not an S32, and is refused rather than truncated:
    // a silently rounded argument is a different call.
    assert_eq!(
        refusal(&with("wibble(2.5, true)")),
        EffectError::ArgType {
            attr: "onGrommet".into(),
            effect: "wibble".into(),
            index: 0,
            declared: TypeShape::S32,
        }
    );
    // Not a literal at all. An effect call is not an expression language; a
    // computation is a Module, referenced opaquely (Rule 46a).
    for arg in ["props.destination", "1 + 2", "f()", "`sprocket`"] {
        assert_eq!(
            refusal(&with(&format!("frobnicate({arg})"))),
            EffectError::ArgNotALiteral {
                attr: "onGrommet".into(),
                effect: "frobnicate".into(),
                index: 0,
            },
            "`{arg}` was not refused as a non-literal",
        );
    }
}

/// **Refusal 4.** A host namespace bound by `import * as` (or a default
/// import): `fx.frobnicate(...)` cannot reach a flat callee except as the
/// string `"fx.frobnicate"`, which is structure smuggled into a name.
#[test]
fn a_host_namespace_bound_as_a_namespace_is_refused() {
    assert_eq!(
        refusal(&format!(
            r#"
            import * as fx from "{ZORK}";
            <Widget id="a" onGrommet={{fx.frobnicate("sprocket")}} />
            "#
        )),
        EffectError::NotANamedImport {
            source: ZORK.into(),
            local: "fx".into(),
            kind: ImportKind::Namespace,
        }
    );
    assert_eq!(
        refusal(&format!(
            r#"
            import fx from "{ZORK}";
            <Widget id="a" onGrommet={{fx("sprocket")}} />
            "#
        )),
        EffectError::NotANamedImport {
            source: ZORK.into(),
            local: "fx".into(),
            kind: ImportKind::Default,
        }
    );
    // The refusal is about the IMPORT, so it fires whether or not anything
    // calls through it - a binding that could never name an effect is a mistake
    // at the line that wrote it.
    assert_eq!(
        refusal(&format!(
            r#"
            import * as fx from "{ZORK}";
            <Widget id="a" />
            "#
        )),
        EffectError::NotANamedImport {
            source: ZORK.into(),
            local: "fx".into(),
            kind: ImportKind::Namespace,
        }
    );
}

/// **Rule 48 at the import line.** A `host:` specifier names a granted
/// namespace and a name that namespace declares, or it is refused at load -
/// rather than producing a binding that can never fire.
#[test]
fn a_host_import_is_granted_or_refused() {
    // A `host:` specifier the provider does not resolve.
    assert_eq!(
        refusal(
            r#"
            import { frobnicate } from "host:telemetry";
            <Widget id="a" />
            "#
        ),
        EffectError::UnknownHostNamespace {
            source: "host:telemetry".into(),
        }
    );
    // A granted namespace that does not declare this name.
    assert_eq!(
        refusal(&format!(
            r#"
            import {{ teleport }} from "{ZORK}";
            <Widget id="a" />
            "#
        )),
        EffectError::UndeclaredHostImport {
            source: ZORK.into(),
            imported: "teleport".into(),
        }
    );
    // A `host:` specifier the provider resolves to something COMPILED is not
    // granted either: a capability with a source behind it is not a capability.
    struct Muddled(Vec<FuncSig>);
    impl ParserHost for Muddled {
        fn resolve(&self, specifier: &str) -> Option<Resolution<'_>> {
            match specifier {
                "host:muddle" => Some(Resolution::Script),
                ZORK => Some(Resolution::Host(&self.0)),
                _ => None,
            }
        }
    }
    assert_eq!(
        ParseCtx::builder()
            .set_host(Muddled(vec![sig(
                "frobnicate",
                vec![param("sprocket", TypeShape::String)]
            )]))
            .build()
            .parse_tsx(
                r#"
                import { frobnicate } from "host:muddle";
                <Widget id="a" />
                "#
            ),
        Err(ParseError::Effect(EffectError::UnknownHostNamespace {
            source: "host:muddle".into(),
        }))
    );
}

/// **Nothing granted is the honest default.** [`parse_tsx`] offers no host at
/// all, so a source that calls an effect has named a capability it was not
/// given - and says so, rather than erasing the call.
///
/// The two "nothings" are different facts and say so separately (Rule 49): a
/// context with the surface *enabled* and this namespace ungranted is
/// [`EffectError::UnknownHostNamespace`]; the default context, which offers no
/// surface at all, is [`EffectError::EffectsNotOffered`].
#[test]
fn with_nothing_granted_an_effect_does_not_resolve() {
    let src = format!(
        r#"
        import {{ frobnicate }} from "{ZORK}";
        <Widget id="a" onGrommet={{frobnicate("sprocket")}} />
        "#
    );
    assert_eq!(
        ParseCtx::builder().enable_effects().build().parse_tsx(&src),
        Err(ParseError::Effect(EffectError::UnknownHostNamespace {
            source: ZORK.into(),
        }))
    );
    assert_eq!(
        ParseCtx::default().parse_tsx(&src),
        Err(ParseError::Effect(EffectError::EffectsNotOffered {
            source: ZORK.into(),
        }))
    );
    // Through the untyped entry point the same refusal arrives as a message, so
    // no caller silently gets a document.
    let messages = parse_tsx(&src).expect_err("refused");
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains(ZORK), "{messages:?}");
    assert!(messages[0].is_ascii(), "{messages:?}");

    // A source with no host import at all is unaffected by the rule, in every
    // context - including a Script import, which a load with no provider cannot
    // resolve and has never had to.
    assert!(parse_tsx(r#"<Screen name="Home"><Item /></Screen>"#).is_ok());
    assert!(
        ParseCtx::default()
            .parse_tsx(&format!(
                r#"
                import {{ frobnicate }} from "{PLOVER}";
                <Widget id="a" />
                "#
            ))
            .is_ok()
    );
}

/// **What the parser produced is what serializes**, effects included - the one
/// attribute value with a nested shape.
#[test]
fn a_parsed_effect_survives_serde() {
    let doc = ctx()
        .parse_tsx(&format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            <Panel id="d1"><Widget id="d2" onGrommet={{frobnicate("sprocket")}} /></Panel>
            "#
        ))
        .expect("parses");
    let json = serde_json::to_string(&doc).expect("serialize");
    let back: TsxDocument = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(doc, back);

    let Node::Element(panel) = &back.root_nodes[0] else {
        panic!()
    };
    let Node::Element(row) = &panel.children[0] else {
        panic!()
    };
    assert_eq!(
        row.attr("onGrommet"),
        Some(&AttrValue::NamedEffect(NamedEffect {
            name: "frobnicate".into(),
            args: vec![Expr::LitStr("sprocket".into())],
        })),
        "the effect did not survive the round trip",
    );
}

/// **RULE 49's whole argument.** One context, configured once, serves every
/// entry point - so a capability enabled for `parse_tsx` is available to
/// `parse_app` without anybody extending a third function.
///
/// The multi-file path could not express an effect at all before this: its
/// predecessor called the ungranted `parse_tsx`, so a screen file's effect was
/// refused however the embedding was configured. Both halves are asserted here,
/// because "the app parses" on its own would also pass if the parse had simply
/// become lenient.
#[test]
fn one_context_serves_parse_app_as_well_as_parse_tsx() {
    let app = "<App />";
    let screen = format!(
        r#"
        import {{ frobnicate }} from "{ZORK}";
        <Screen name="Home"><Widget id="a" onGrommet={{frobnicate("sprocket")}} /></Screen>
        "#
    );

    let doc = ctx()
        .parse_app(app, &[&screen])
        .expect("the grant reaches a screen file");
    let Node::Element(root) = &doc.root_nodes[0] else {
        panic!("the app root is an element")
    };
    let Node::Element(spliced) = &root.children[0] else {
        panic!("the screen is spliced in as an element")
    };
    let Node::Element(widget) = &spliced.children[0] else {
        panic!("the widget is the screen's child")
    };
    assert_eq!(
        widget.attr("onGrommet"),
        Some(&AttrValue::NamedEffect(NamedEffect {
            name: "frobnicate".into(),
            args: vec![Expr::LitStr("sprocket".into())],
        })),
        "the effect did not survive the splice",
    );

    // And the default context still refuses the same source, so what made the
    // difference was the provider rather than a lenient parse.
    assert_eq!(
        ParseCtx::default().parse_app(app, &[&screen]),
        Err(ParseError::Effect(EffectError::EffectsNotOffered {
            source: ZORK.into(),
        }))
    );
}

/// **A spread attribute is refused** (Rule 46a's remaining door).
///
/// `const handlers = { onGrommet: frobnicate("sprocket") }` then
/// `<Widget {...handlers}/>` used to parse clean and yield a `<Widget>` with
/// **no binding**: the attribute loop only ever saw
/// `JSXAttributeItem::Attribute`, so a spread fell off the end and the
/// event-binding recognition was never consulted. That is the erasure the
/// `NamedEffect` producer exists to close, arriving by the one route none of
/// its refusals watch.
#[test]
fn a_spread_attribute_is_refused() {
    // The erasure itself: an effect that reaches the element as nothing.
    assert_eq!(
        refusal(&format!(
            r#"
            import {{ frobnicate }} from "{ZORK}";
            const handlers = {{ onGrommet: frobnicate("sprocket") }};
            <Widget id="a" {{...handlers}} />
            "#
        )),
        EffectError::SpreadAttribute {
            tag: "Widget".into()
        }
    );
    // The refusal is about the SPREAD, not about effects: an attribute set
    // spread from a value cannot be checked against declared props either, so
    // it is refused with nothing granted and on a nested element too.
    assert_eq!(
        ParseCtx::default().parse_tsx(r#"<Screen name="Home"><Item {...props} /></Screen>"#),
        Err(ParseError::Effect(EffectError::SpreadAttribute {
            tag: "Item".into()
        }))
    );
    // An ordinary attribute list is untouched by the rule.
    assert!(parse_tsx(r#"<Item id="a" label="Hi" />"#).is_ok());
}

/// Every refusal renders ASCII (Rule 39): these strings reach the editor's
/// live-parse status strip and panic dumps.
#[test]
fn every_effect_refusal_renders_ascii() {
    for e in [
        EffectError::NotACall {
            attr: "onGrommet".into(),
        },
        EffectError::Unresolved {
            attr: "onGrommet".into(),
            callee: "frobnicate".into(),
        },
        EffectError::NotAHostImport {
            attr: "onGrommet".into(),
            callee: "frobnicate".into(),
            source: PLOVER.into(),
        },
        EffectError::ArgCount {
            attr: "onGrommet".into(),
            effect: "frobnicate".into(),
            declared: 1,
            given: 0,
        },
        EffectError::ArgType {
            attr: "onGrommet".into(),
            effect: "frobnicate".into(),
            index: 0,
            declared: TypeShape::String,
        },
        EffectError::ArgNotALiteral {
            attr: "onGrommet".into(),
            effect: "frobnicate".into(),
            index: 0,
        },
        EffectError::NotANamedImport {
            source: ZORK.into(),
            local: "fx".into(),
            kind: ImportKind::Namespace,
        },
        EffectError::UnknownHostNamespace {
            source: "host:telemetry".into(),
        },
        EffectError::UndeclaredHostImport {
            source: ZORK.into(),
            imported: "teleport".into(),
        },
        EffectError::EffectsNotOffered { source: ZORK.into() },
        EffectError::GrantedWithoutScheme {
            source: "zork".into(),
        },
        EffectError::SpreadAttribute {
            tag: "Widget".into(),
        },
    ] {
        assert!(e.to_string().is_ascii(), "{e:?}");
        assert!(!e.to_string().is_empty());
    }
}
