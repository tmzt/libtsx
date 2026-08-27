//! **THE CONST ARROW, pinned as it behaves TODAY** - COMPONENT_DECLARATION.md
//! step 7, parse only, no renderer anywhere.
//!
//! `const Swatch = (props: SwatchProps) => (<Box/>)` already parses. What it
//! does NOT do is declare anything: pass 1 collects the arrow into
//! `arrow_components` (`src/parse.rs`, the `Statement::VariableDeclaration`
//! arm), uses the NAME only to match an `export default Name`, drops it, and
//! never reads the PARAMETER at all. Those two discards are precisely what
//! stands between the syntax and a declaration.
//!
//! Every assertion below is GREEN today and says, in its own doc, what it
//! becomes when step 8 lands. That is the point: a measurement that outlives
//! the conversation, and a failure that names the feature rather than a
//! regression.
//!
//! # What was measured, and where the plan was wrong
//!
//! Step 7 was written expecting to be RED ("it fails today on the name, the
//! parameter and the props spelling"). Measured, at the `TsxDocument` level it
//! is GREEN, and that is the more useful answer:
//!
//! * **The lowering equality ALREADY HOLDS.** A const-arrow source and its
//!   bare-JSX twin parse to `TsxDocument`s that are equal - not nearly equal,
//!   equal - because everything the arrow adds is thrown away. So D5's claim
//!   ("a const arrow LOWERS; the `.hbdef` shape does not move") is a checked
//!   fact from this file forward, and step 8 is a lowering rather than a format
//!   change. The 22 committed `.hbdef` are safe BY CONSTRUCTION.
//! * **The destructuring claim inverts.** The plan expected step 7 to assert
//!   that a destructured parameter is still REFUSED. In the component-arrow
//!   position it is not refused - it is silently ACCEPTED, because no parameter
//!   is read at all. The refusal (`arrow parameters must be simple
//!   identifiers`) lives in `arrow_params`, which only the BINDING-EXPRESSION
//!   arrow reaches. Step 8 does not preserve a refusal here; it introduces one,
//!   to a position that today accepts and ignores. Silently ignoring
//!   `({label}: P)` is worse than refusing it, and is the sharper reason to do
//!   step 8 at all.
//!
//! # What each test becomes at step 8
//!
//! Named in each test's own doc. In one line each:
//!
//! * `the_bare_arrow_and_its_jsx_twin_parse_equal` - STAYS GREEN. It is the
//!   invariant, not the target; if step 8 reddens it, the persisted shape moved
//!   and 22 fixtures need regenerating.
//! * `the_parameterised_arrow_is_equal_only_because_the_parameter_is_dropped`
//!   - GOES RED. After step 8 the parameter's type lands on the root as
//!   `props=`, so this document stops being equal to a bare twin that declares
//!   no props.
//! * `the_arrows_own_name_reaches_nothing` - GOES RED. `Definition.symbol`
//!   comes from the arrow, not from a caller's string literal.
//! * `the_parameters_type_reaches_nothing` - GOES RED. `props_shape()` resolves
//!   `SwatchProps` through the interfaces parsed from the same source.
//! * `a_destructured_component_parameter_is_accepted_and_ignored` - GOES RED,
//!   as an `Err`: the refusal that today only guards binding expressions
//!   reaches the component arrow.
//! * `an_exported_const_arrow_is_invisible` - GOES RED at step 10, when the
//!   arrow loop learns `ExportNamedDeclaration`.
//! * `props_dot_x_and_uicomponent_props_dot_x_are_different_expressions_today`
//!   - GOES RED. `props.x` is rewritten to `uiComponent().props.x` at parse.
//! * `only_the_first_const_arrow_survives_a_module_with_no_export` - GOES RED
//!   at step 10 (rules 3 and 4: every other export, and private consts).
//!
//! **Gated on `parse`**, like `parser_host.rs` and `binding_expr_seam.rs`:
//! without the gate this target fails to COMPILE under
//! `--no-default-features`, which is the configuration `highbay_data` consumes.

#![cfg(feature = "parse")]

use libtsx::dag::{AttrValue, BindingExpr, Definition, Element, InterfaceDecl, Node};
use libtsx::{extract_interfaces, parse_tsx};

/// The one element every source below returns, spelled identically in each, so
/// that a difference between two parses is a difference the DECLARATION made.
const BODY: &str = "<Box width={64} height={64} />";

fn parse(source: &str) -> libtsx::TsxDocument {
    parse_tsx(source).unwrap_or_else(|e| panic!("must parse:\n{source}\n{e:?}"))
}

fn interfaces(source: &str) -> Vec<InterfaceDecl> {
    extract_interfaces(source).unwrap_or_else(|e| panic!("must parse:\n{source}\n{e:?}"))
}

/// The single root element, refused loudly if the document is not one element.
fn root(source: &str) -> Element {
    match parse(source).root_nodes.as_slice() {
        [Node::Element(e)] => e.clone(),
        other => panic!("expected exactly one root element, got {other:?}"),
    }
}

/// Serde is the mechanical form of "appears nowhere in the result": the whole
/// owned document as text, so a name surviving in ANY field - a tag, an attr
/// key, a binding path, a type argument - is caught, rather than the handful of
/// fields a hand-written walk remembered to look at.
fn as_text(doc: &libtsx::TsxDocument) -> String {
    serde_json::to_string(doc).expect("TsxDocument is Serialize")
}

/// **THE D5 EQUALITY, and it already holds.**
///
/// `const Swatch = () => (<Box/>)` and a bare `<Box/>` parse to EQUAL
/// documents. This is what makes step 8 a lowering: the persisted shape does
/// not move, so none of the 22 committed `.hbdef` needs regenerating and
/// `HBDEF_VERSION` stays at 3.
///
/// **This test STAYS GREEN through step 8** - it is the invariant the feature
/// must not break, not the behaviour the feature replaces. A no-parameter arrow
/// declares nothing but its name, and its name is not part of the document.
/// If this reddens, the lowering claim was wrong and the fixture cost the plan
/// avoided has come due.
#[test]
fn the_bare_arrow_and_its_jsx_twin_parse_equal() {
    let twin = parse(&format!("{BODY}\n"));
    let arrow = parse(&format!("const Swatch = () => ({BODY});\n"));
    assert_eq!(
        arrow, twin,
        "a const arrow must lower to exactly its bare-JSX twin. This is D5 as a \
         checked fact rather than a claim in a commit message.",
    );

    // The three ways an author writes the same component, all one document.
    let via_default = parse(&format!("const Swatch = () => ({BODY});\nexport default Swatch;\n"));
    let inline_default = parse(&format!("export default () => ({BODY});\n"));
    assert_eq!(via_default, twin, "export-default-by-name lowers the same way");
    assert_eq!(inline_default, twin, "an inline default arrow lowers the same way");
}

/// **The parameterised arrow is equal to the twin too - and that is the bug.**
///
/// `const Swatch = (props: SwatchProps) => (<Box/>)` parses to the SAME
/// document as a bare `<Box/>` that declares no props at all. The author wrote
/// a props shape; the parse discarded it; the two are indistinguishable
/// afterwards.
///
/// **GOES RED at step 8**, and the failure is the feature: the parameter's type
/// lowers onto the root's reserved `props=` attribute, so the parameterised
/// arrow becomes equal to `<Box props="SwatchProps" .../>` and stops being
/// equal to the undeclared twin. Update this test by moving the `assert_eq!`
/// onto the props-declaring twin below, which is already written out here so
/// the shape step 8 must produce is on the record.
#[test]
fn the_parameterised_arrow_is_equal_only_because_the_parameter_is_dropped() {
    let undeclared = parse(&format!("{BODY}\n"));
    let declared = parse(&format!(
        "interface SwatchProps {{ width: number; }}\n\
         const Swatch = (props: SwatchProps) => ({BODY});\n"
    ));
    assert_eq!(
        declared, undeclared,
        "TODAY a declared props shape changes NOTHING about the parsed document. \
         At step 8 this must fail, and equal the props-declaring twin instead.",
    );

    // The twin step 8 has to produce, parsed here so the target shape is
    // measured rather than imagined. Today nothing produces it from an arrow.
    let target_twin = root("<Box props=\"SwatchProps\" width={64} height={64} />\n");
    assert_eq!(
        target_twin.attr("props"),
        Some(&AttrValue::Str("SwatchProps".into())),
        "the reserved props attribute is the slot the parameter's type lowers onto",
    );
    assert_ne!(
        root(&format!("const Swatch = (props: SwatchProps) => ({BODY});\n")),
        target_twin,
        "TODAY the arrow does not reach that slot. This inequality IS step 8's work.",
    );
}

/// **The arrow's own name reaches nothing.**
///
/// `Swatch` is used to match an `export default Swatch` and is then dropped. It
/// is not a field, not an attribute, not a tag - the string does not occur
/// anywhere in the serialized document. So today the ONLY route a symbol takes
/// is a caller's string literal at `Definition::from_document`, of which the
/// plan counts about twenty, and nothing in the parse can contradict a wrong
/// one: a source whose const says `Swatch` yields a definition called
/// `TotallyWrong` without complaint.
///
/// **GOES RED at step 8** (naming rules 1-2): the symbol comes from the arrow
/// or from the file stem, and `from_document`'s literal argument is retired.
#[test]
fn the_arrows_own_name_reaches_nothing() {
    let doc = parse(&format!("const Swatch = () => ({BODY});\n"));
    assert!(
        !as_text(&doc).contains("Swatch"),
        "the const's name must be absent from the whole document today: {}",
        as_text(&doc),
    );

    let def = Definition::from_document("TotallyWrong", doc, Vec::new())
        .expect("one root element is one definition");
    assert_eq!(
        def.symbol, "TotallyWrong",
        "TODAY a definition is named by its CALLER and the source cannot disagree. \
         At step 8 the name comes from the arrow (or the file stem) and this literal \
         has nowhere left to enter.",
    );
}

/// **The parameter's type reaches nothing either** - the row
/// COMPONENT_DECLARATION.md calls the real prize.
///
/// `SwatchProps` is declared in the source, named as the parameter's type, and
/// absent from the parsed document. It survives only as a free-standing
/// `InterfaceDecl` from `extract_interfaces`, with nothing connecting it to the
/// component: `props_name()` is `None`, so `props_shape()` is `None` even
/// though the interface is sitting right there in the same `Definition`.
///
/// Nothing new is needed to fix this. `BindingParam` is already
/// `{ name, ty: TypeShape }`, so the parameter's type is a `TypeShape` the
/// moment `arrow_params` is called on a component arrow - which today it never
/// is.
///
/// **GOES RED at step 8**: `props_name()` becomes `Some("SwatchProps")` and
/// `props_shape()` resolves to the interface below.
#[test]
fn the_parameters_type_reaches_nothing() {
    let source = format!(
        "interface SwatchProps {{ width: number; }}\n\
         const Swatch = (props: SwatchProps) => ({BODY});\n"
    );
    let doc = parse(&source);
    assert!(
        !as_text(&doc).contains("SwatchProps"),
        "the parameter's type must be absent from the whole document today: {}",
        as_text(&doc),
    );

    // The interface IS parsed - it is only the LINK that is missing, which is
    // what makes step 8 a wiring job rather than a type job.
    let ifaces = interfaces(&source);
    assert_eq!(ifaces.len(), 1, "the interface itself parses fine");
    assert_eq!(ifaces[0].name, "SwatchProps");

    let def = Definition::from_document("Swatch", doc, ifaces)
        .expect("one root element is one definition");
    assert_eq!(
        def.props_name(),
        Ok(None),
        "TODAY the definition declares no props shape, though the author wrote one. \
         At step 8 this is Some(\"SwatchProps\").",
    );
    assert!(
        matches!(def.props_shape(), Ok(None)),
        "and so it resolves to nothing, with the interface it should have resolved \
         to held in the very same definition. At step 8 this is the InterfaceDecl.",
    );
}

/// **A destructured component parameter is ACCEPTED and IGNORED** - the claim
/// step 7 was written to pin, inverted by measurement.
///
/// The plan expected "the destructuring refusal still stands". It does not
/// stand HERE. `const Swatch = ({ label }: SwatchProps) => (<Box/>)` parses
/// clean and yields the same document as every other spelling, because the
/// component path reads no parameters and therefore has nothing to refuse. A
/// rest parameter is accepted the same way.
///
/// The refusal is real, and it is somewhere else: `arrow_params` guards the
/// BINDING-EXPRESSION arrow - the `x => x` inside an attribute - and both its
/// messages are asserted below so a reworded one is a visible event. Note what
/// the messages do and do not say: each states the REQUIREMENT, and the reason
/// lives in the `BindingExpr::Arrow` doc in `src/dag.rs` ("a destructuring or
/// rest parameter is refused"), not in the string an author sees.
///
/// **GOES RED at step 8, as an `Err`.** Step 8 does not preserve a refusal in
/// this position - it EXTENDS one to a position that today accepts and drops.
/// That is the stronger argument for step 8 than the syntax is: silently
/// ignoring a destructured props parameter is worse than refusing it.
#[test]
fn a_destructured_component_parameter_is_accepted_and_ignored() {
    let twin = parse(&format!("{BODY}\n"));
    for parameter in ["{ label }: SwatchProps", "...rest"] {
        let source = format!("const Swatch = ({parameter}) => ({BODY});\n");
        assert_eq!(
            parse(&source),
            twin,
            "TODAY `({parameter})` parses to the plain twin - no error, no trace of \
             the parameter. At step 8 this must be an Err naming the parameter.",
        );
    }

    // Where the refusal DOES stand today, with its exact wording.
    assert_eq!(
        BindingExpr::try_from("({a}) => a").unwrap_err().to_string(),
        "unsupported binding expression: arrow parameters must be simple identifiers",
        "the refusal guards the binding-expression arrow, and only that one",
    );
    assert_eq!(
        BindingExpr::try_from("(...a) => a").unwrap_err().to_string(),
        "unsupported binding expression: arrow rest parameters are unsupported",
    );
    assert!(
        BindingExpr::try_from("x => x").is_ok(),
        "a simple identifier parameter is the accepted form, in that position",
    );
}

/// **`export const Chip = () => (<JSX/>)` is INVISIBLE.**
///
/// The arrow loop matches a bare `Statement::VariableDeclaration`; an exported
/// one arrives wrapped in `Statement::ExportNamedDeclaration` and falls through
/// every arm. The result is not an error - it is an EMPTY document. A file
/// whose only component is exported the way most authors would write it parses
/// successfully to nothing at all.
///
/// The unwrapping precedent is in the same file: `extract_interfaces` reads
/// `interface` and `export interface` through exactly that match, which is why
/// the interface below IS found in the very same source that yields no roots.
///
/// **GOES RED at step 10** (naming rule 3, every other export is its own
/// component), when the arrow loop learns the wrapper.
#[test]
fn an_exported_const_arrow_is_invisible() {
    let source = format!(
        "interface ChipProps {{ width: number; }}\n\
         export const Chip = (props: ChipProps) => ({BODY});\n"
    );
    let doc = parse(&source);
    assert!(
        doc.root_nodes.is_empty(),
        "TODAY an exported const arrow contributes NO root node and no error: {:?}. \
         At step 10 this is one component named Chip.",
        doc.root_nodes,
    );
    assert!(
        matches!(Definition::from_document("Chip", doc, Vec::new()), Err(libtsx::DefError::NoUi)),
        "so the only complaint arrives later, as `no UI`, naming neither the export \
         nor the reason",
    );

    // The same statement wrapper, on the same kind of declaration, IS unwrapped
    // one function over. That asymmetry is the whole of step 10's parse change.
    assert_eq!(
        interfaces(&source).len(),
        1,
        "`export interface` is read through ExportNamedDeclaration already - the \
         precedent the arrow loop has not adopted",
    );
}

/// **`props.width` and `uiComponent().props.width` are DIFFERENT expressions
/// today**, and the first one puts an unresolved local name into the graph.
///
/// The arrow's parameter is called `props`, so `props.width` inside the body is
/// a reference to it - but since no parameter is read, nothing rewrites the
/// name and it lowers as a bare `Path(["props", "width"])`: the author's local
/// binding, persisted verbatim, resolvable by nothing downstream.
///
/// **GOES RED at step 8**, where rewriting `Named(param)` to `UiComponent` AT
/// PARSE is what keeps the local name out of the graph. The assertion becomes
/// `assert_eq!`, which is COMPONENT_DECLARATION.md's item 6.
#[test]
fn props_dot_x_and_uicomponent_props_dot_x_are_different_expressions_today() {
    let shorthand = root("const Swatch = (props: SwatchProps) => (<Box width={props.width} />);\n");
    let resolved = root("<Box width={uiComponent().props.width} />\n");

    assert_eq!(
        shorthand.attr("width"),
        Some(&AttrValue::BindingExpr(BindingExpr::Path(vec![
            "props".into(),
            "width".into()
        ]))),
        "TODAY the parameter's own name survives into the graph as a plain path",
    );
    assert_ne!(
        shorthand.attr("width"),
        resolved.attr("width"),
        "TODAY the two spellings mean different things. At step 8 they are the same \
         expression and this becomes an assert_eq.",
    );
}

/// **A module with several const arrows and no export keeps only the FIRST.**
///
/// The lenient fallback exists so a mid-edit missing `export default` does not
/// blank the preview, and it takes `arrow_components.first()`. With two
/// components in one file that is a silent choice: the second is dropped with
/// no error, no diagnostic, and no way for the author to tell which one the
/// preview is showing.
///
/// **GOES RED at step 10** (naming rules 3 and 4), which is the step where one
/// file may hold several components - and where D6 has to say what artifact
/// they land in, since a private const has neither an export name nor a path to
/// derive a filename from.
#[test]
fn only_the_first_const_arrow_survives_a_module_with_no_export() {
    let doc = parse(&format!(
        "const Chip = () => (<Chip1 />);\nconst Swatch = () => ({BODY});\n"
    ));
    assert_eq!(
        doc.root_nodes.len(),
        1,
        "TODAY two components in one file yield one root",
    );
    assert!(
        matches!(&doc.root_nodes[0], Node::Element(e) if e.tag == "Chip1"),
        "and it is the FIRST arrow, chosen by source order rather than by anything \
         the author said: {:?}",
        doc.root_nodes,
    );
}
