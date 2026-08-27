//! TSX source emitter — converts a parsed [`TsxDocument`] back to TSX source text.
//!
//! This module emits a [`TsxDocument`] as TSX source code, preserving the structure,
//! attributes, type arguments, and child nodes in source order. The output may differ
//! in formatting from the original source, but the semantic structure is identical.
//!
//! Comments **are** preserved, when the parse that produced the document was asked
//! to keep them ([`crate::ParseCtxBuilder::retain_comments`]). A [`Node::Comment`]
//! holds the comment verbatim, delimiters included, so emitting one is a copy: the
//! only thing this module decides is the punctuation AROUND it, which differs by
//! position (a comment at the top level of the module is written as-is; a comment
//! among JSX children has to be wrapped in the `{ }` that JSX requires).
//!
//! A document from a parse that did NOT retain comments carries none, and emits
//! none - which is the publish path, and is why no `.hbdef` can contain one.

use crate::dag::{
    AttrValue, BindingExpr, BindingParam, BlockArrow, BlockStmt, Element,
    ImportDecl, InterfaceDecl, LiteralValue, Node, TsxDocument, TypeShape,
};

/// Emit a [`TsxDocument`] back to TSX source text.
///
/// The output preserves the semantic structure of the document: imports in order,
/// interfaces in order, and the element tree with all attributes and children in
/// their authored order. Type arguments, imported calls, and all attribute value
/// variants are correctly re-emitted.
pub fn emit_tsx_document(doc: &TsxDocument) -> String {
    let mut out = String::new();

    // Emit imports first
    for import in &doc.imports {
        emit_import(&mut out, import);
        out.push('\n');
    }

    // Add spacing after imports if there are any
    if !doc.imports.is_empty() && !doc.root_nodes.is_empty() {
        out.push('\n');
    }

    // Emit root nodes. `Position::Module` because these are statements of the
    // module, not children of an element - which is the whole of what a
    // comment's punctuation depends on.
    for node in &doc.root_nodes {
        emit_node(&mut out, node, 0, Position::Module);
    }

    out
}

/// Where a node is being emitted, which is what decides how a comment is
/// punctuated.
///
/// The two are not interchangeable and getting it wrong is silent: `// note`
/// written among JSX children is not a comment at all, it is TEXT, and would
/// re-parse as a [`Node::Text`] that then draws. Making the caller state the
/// position is what stops that being a judgement call at each of the two call
/// sites.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    /// A statement of the module: comments are written as authored.
    Module,
    /// A child of a JSX element: comments must be wrapped in `{ }`.
    JsxChild,
}

/// Emit an import declaration.
fn emit_import(out: &mut String, import: &ImportDecl) {
    out.push_str("import { ");

    for (i, name) in import.names.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }

        out.push_str(&name.imported);

        // Emit alias if local name differs from imported name
        if name.local != name.imported {
            out.push_str(" as ");
            out.push_str(&name.local);
        }
    }

    out.push_str(" } from \"");
    out.push_str(&import.source);
    out.push_str("\";");
}

/// Emit a node with the given indentation level, punctuated for `position`.
fn emit_node(out: &mut String, node: &Node, indent: usize, position: Position) {
    match node {
        Node::Element(elem) => {
            emit_element(out, elem, indent);
            out.push('\n');
        }
        Node::Text(text) => {
            // For text nodes, emit the text as-is wrapped in braces to preserve formatting.
            // This ensures round-trip compatibility with the parser.
            out.push('{');
            out.push('"');
            out.push_str(text);
            out.push('"');
            out.push('}');
            out.push('\n');
        }
        Node::Expr(expr) => {
            out.push('{');
            out.push_str(expr);
            out.push('}');
            out.push('\n');
        }
        Node::Comment(text) => emit_comment(out, text, indent, position),
    }
}

/// Emit a comment - the text VERBATIM, with only the punctuation its position
/// requires added around it.
///
/// Nothing here rewrites the comment: a `//` stays a `//` and a `/* */` stays a
/// `/* */`, because the parse stored what was authored and re-deriving a
/// delimiter is how `parse -> emit -> parse` stops being the identity (see
/// [`Node::Comment`]).
///
/// Among JSX children the wrapping `{ }` is not cosmetic - it is what makes the
/// text a comment rather than rendered text - and a LINE comment additionally
/// needs the closing brace on the next line, because `//` runs to end of line
/// and `{// note}` comments out the brace that was meant to close the
/// container.
fn emit_comment(out: &mut String, text: &str, indent: usize, position: Position) {
    emit_indent(out, indent);
    match position {
        Position::Module => {
            out.push_str(text);
        }
        Position::JsxChild => {
            out.push('{');
            out.push_str(text);
            if text.starts_with("//") {
                out.push('\n');
                emit_indent(out, indent);
            }
            out.push('}');
        }
    }
    out.push('\n');
}

/// Emit an element with the given indentation level.
fn emit_element(out: &mut String, elem: &Element, indent: usize) {
    emit_indent(out, indent);
    out.push('<');
    out.push_str(&elem.tag);

    // Emit type arguments if any
    if !elem.type_args.is_empty() {
        out.push('<');
        for (i, ty) in elem.type_args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_type_shape(out, ty);
        }
        out.push('>');
    }

    // Emit attributes in source order
    for (name, value) in &elem.attrs {
        out.push(' ');
        out.push_str(name);
        emit_attr_value(out, value);
    }

    // Check if element has children
    if elem.children.is_empty() {
        // Self-closing element
        out.push_str(" />");
    } else {
        out.push('>');
        out.push('\n');

        // Emit children with increased indentation
        for child in &elem.children {
            emit_node(out, child, indent + 1, Position::JsxChild);
        }

        // Emit closing tag
        emit_indent(out, indent);
        out.push_str("</");
        out.push_str(&elem.tag);
        out.push('>');
    }
}

/// Emit an attribute value.
fn emit_attr_value(out: &mut String, value: &AttrValue) {
    match value {
        AttrValue::Str(s) => {
            out.push_str("=\"");
            out.push_str(s);
            out.push('"');
        }
        AttrValue::Num(n) => {
            out.push_str("={");
            // Format number carefully
            if n.fract() == 0.0 && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 {
                out.push_str(&format!("{}", *n as i32));
            } else {
                out.push_str(&n.to_string());
            }
            out.push('}');
        }
        AttrValue::Bool(b) => {
            if *b {
                out.push_str("={true}");
            } else {
                out.push_str("={false}");
            }
        }
        AttrValue::Binding(expr) => {
            out.push_str("={");
            out.push_str(expr);
            out.push('}');
        }
        AttrValue::Opaque => {
            // Opaque values are not re-emitted — they cannot be reconstructed
            // from the parse. This is a gap in the forward direction; callers
            // seeking to serialize will need to track these separately or accept
            // their loss.
        }
        AttrValue::BindingExpr(expr) => {
            out.push_str("={");
            emit_binding_expr(out, expr);
            out.push('}');
        }
        AttrValue::ImportedCall(call) => {
            out.push_str("={");
            // Emit just the imported name, not the namespace.
            // The namespace is established by the import and should not appear
            // in the JSX expression (e.g., emit "navigate(...)" not "host:effects.navigate(...)")
            out.push_str(&call.name);
            out.push('(');

            for (i, arg) in call.args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(out, arg);
            }
            out.push_str(")}");
        }
    }
}

/// **The public emit seam: one expression node to its TypeScript text.**
///
/// The downward rung of the IR ladder, spelled as Rust's own conversion trait
/// rather than a bespoke verb. Emit is TOTAL - every [`BindingExpr`] has a text
/// - so it is [`From`] and not [`TryFrom`], and the pairing with
/// [`BindingExpr`]'s `TryFrom<&str>` (feature `parse`) is what makes the
/// round-trip property statable as a law: **`From` and `TryFrom` are inverse
/// over the image of the parse.**
///
/// ```
/// use libtsx::dag::{BindingExpr, LiteralValue};
///
/// let expr = BindingExpr::Coalesce(vec![
///     BindingExpr::Path(vec!["props".into(), "label".into()]),
///     BindingExpr::Literal(LiteralValue::String("none".into())),
/// ]);
/// assert_eq!(String::from(&expr), r#"props.label ?? "none""#);
/// ```
///
/// **No feature gate.** [`crate::emit`] names no `oxc_*` type and never has, so
/// this door is open to a `default-features = false` consumer that holds a
/// graph and wants source out of it - unlike the parse door, which needs oxc to
/// exist at all.
///
/// **Orphan rule**: legal here and only here. `String` is std's, but `&T` is
/// fundamental, so `&BindingExpr` counts as local to the crate that defines
/// [`BindingExpr`] - which is this one. The same impl written from a peer crate
/// would have two foreign ends and would not compile; each rung of the ladder
/// belongs to the crate that owns its NEW type.
///
/// The `&mut String` writer beside this one stays private: it is the recursion,
/// and the shared buffer is why (an expression is spliced into a document, an
/// attribute, an argument list).
impl From<&BindingExpr> for String {
    fn from(expr: &BindingExpr) -> Self {
        let mut out = String::new();
        emit_binding_expr(&mut out, expr);
        out
    }
}

/// Emit one owned object-binding expression. Unlike [`AttrValue::ImportedCall`],
/// this vocabulary is not used by the legacy event parser; it is emitted only
/// when a caller has already constructed the owned semantic IR.
///
/// The public spelling is `impl From<&BindingExpr> for String` above; this is
/// the writer it and every internal splice share.
fn emit_binding_expr(out: &mut String, expr: &BindingExpr) {
    match expr {
        BindingExpr::Literal(literal) => match literal {
            LiteralValue::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            LiteralValue::Int32(value) => out.push_str(&value.to_string()),
            LiteralValue::Int64(value) => out.push_str(&value.to_string()),
            LiteralValue::Float32(value) => push_float_literal(out, f64::from(*value)),
            LiteralValue::Float64(value) => push_float_literal(out, *value),
            LiteralValue::String(value) => push_string_literal(out, value),
        },
        BindingExpr::Null => out.push_str("null"),
        BindingExpr::Path(path) => out.push_str(&path.join(".")),
        BindingExpr::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                emit_binding_expr(out, item);
            }
            out.push(']');
        }
        BindingExpr::Record(fields) => {
            out.push('{');
            for (index, (name, value)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                push_property_key(out, name);
                out.push_str(": ");
                emit_binding_expr(out, value);
            }
            out.push('}');
        }
        BindingExpr::Call {
            namespace,
            name,
            type_args,
            args,
        } => {
            if !namespace.is_empty() {
                out.push_str(namespace);
                out.push('.');
            }
            out.push_str(name);
            if !type_args.is_empty() {
                out.push('<');
                for (index, ty) in type_args.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    emit_type_shape(out, ty);
                }
                out.push('>');
            }
            out.push('(');
            for (index, arg) in args.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                emit_binding_expr(out, arg);
            }
            out.push(')');
        }
        BindingExpr::Async(program) => emit_block_arrow(out, program),
        BindingExpr::Coalesce(operands) => {
            for (index, operand) in operands.iter().enumerate() {
                if index > 0 {
                    out.push_str(" ?? ");
                }
                emit_operand(out, operand);
            }
        }
        BindingExpr::Cond { cond, then, other } => {
            emit_operand(out, cond);
            out.push_str(" ? ");
            emit_operand(out, then);
            out.push_str(" : ");
            emit_operand(out, other);
        }
        BindingExpr::SymbolValue(symbol) => {
            out.push_str(symbol.as_str());
            out.push_str("()");
        }
        BindingExpr::MemberOf(base, member) => {
            emit_member_base(out, base);
            out.push('.');
            out.push_str(member.as_str());
        }
        BindingExpr::Eq {
            left,
            right,
            strict,
        } => {
            emit_operand(out, left);
            out.push_str(if *strict { " === " } else { " == " });
            emit_operand(out, right);
        }
        BindingExpr::Arrow { params, body } => {
            emit_binding_params(out, params);
            out.push_str(" => ");
            emit_arrow_body(out, body);
        }
    }
}

/// **A FLOAT's text, kept a float.**
///
/// `f64`'s `Display` writes `1.0` as `1`, and `1` re-parses as
/// [`LiteralValue::Int64`] - so emitting it bare would break the law this
/// module states, that emit and parse are inverse OVER THE IMAGE OF THE PARSE.
/// The parse now distinguishes an integral token from a decimal one, so the
/// emit has to write a decimal token for a decimal value, whether or not the
/// value happens to be whole.
///
/// The `.0` is appended only when the rendered text is a bare integer -
/// anything with a point, an exponent or a non-finite spelling already
/// re-parses as what it is (or, for a non-finite, is not a TypeScript numeric
/// literal at all and could not have come from a parse).
fn push_float_literal(out: &mut String, value: f64) {
    let text = value.to_string();
    let integral = text
        .strip_prefix('-')
        .unwrap_or(&text)
        .bytes()
        .all(|b| b.is_ascii_digit());
    out.push_str(&text);
    if integral {
        out.push_str(".0");
    }
}

/// **A double-quoted TypeScript string literal for `value`, escaped.**
///
/// The unescaped spelling this replaces pushed `"`, the value, `"`, and failed
/// three ways (libhbui's `codec_round_trip.rs`, F3): a quote ended the literal
/// early, a newline left it unterminated, and a BACKSLASH was silently eaten -
/// `back\slash` emitted as `"back\slash"`, which re-parses as `backslash`. The
/// last is the one that matters, because both of the others are syntax errors a
/// re-parse reports and that one is a different string nothing complains about.
///
/// **Minimal, deliberately.** Only what changes meaning is escaped, so an
/// ordinary string is written the way an author would write it and the round
/// trip is not "escape everything" wearing a fix's clothes - the single quote,
/// the `$`, the backtick and every printable non-ASCII character go through
/// untouched. `U+2028`/`U+2029` are in the list because they are line
/// terminators to a JavaScript lexer even though they look like nothing.
fn push_string_literal(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other if (other as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", other as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// **A property key: bare when it is an identifier, quoted when it is not.**
///
/// The parse accepts both spellings a TypeScript author may write - a
/// `PropertyKey::StaticIdentifier` and a `PropertyKey::StringLiteral` - and
/// stores the same `String` for each, so `{"quoted-key": 1}` is an authorable
/// record whose key no identifier can spell. The emit wrote every key bare,
/// which made `{quoted-key: 1}`: a syntax error, and a shape the parser
/// produces that the emitter could not write (libhbui's `codec_round_trip.rs`,
/// F4).
///
/// Bare stays the DEFAULT and not merely one of two options: quoting every key
/// would round-trip just as well, and would respell every record in the corpus
/// on the next publish.
///
/// Used for a value record's keys and for an inline record TYPE's field names,
/// which take the same two spellings.
fn push_property_key(out: &mut String, name: &str) {
    if is_identifier(name) {
        out.push_str(name);
    } else {
        push_string_literal(out, name);
    }
}

/// Whether `name` can be written bare - ASCII only, deliberately.
///
/// TypeScript admits far more (any `ID_Start` followed by `ID_Continue`s, plus
/// `\u` escapes), and the cost of the narrow answer is one pair of quotes
/// around a key that did not need them - which still re-parses to the same
/// string. The cost of a WIDE answer that is wrong anywhere is emitted text
/// that does not parse, so this errs where the failure is harmless.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let starts = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
    starts && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// `(a: T, b: U)` - the parameter list of either arrow spelling.
///
/// **Always parenthesised, and always annotated.** TypeScript lets a single
/// unannotated parameter drop both (`x => x`), and taking that shortcut would
/// make the emitted text depend on how many parameters there are and on
/// whether a type happens to be known - two more shapes for a re-parse to
/// disagree with, in exchange for two characters.
fn emit_binding_params(out: &mut String, params: &[BindingParam]) {
    out.push('(');
    for (index, param) in params.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&param.name);
        out.push_str(": ");
        emit_type_shape(out, &param.ty);
    }
    out.push(')');
}

/// The body of an expression-bodied arrow, parenthesised where TypeScript would
/// otherwise read it as something else.
///
/// **A LEADING BRACE is the case, not a record body**: `x => {a: 1}` opens a
/// BLOCK, not an object literal, so the body has to be written `x => ({a: 1})`.
/// The hazard belongs to the first TOKEN of the body's emitted text, and a body
/// that merely *begins* with a record has it too - [`emit_operand`] splices a
/// PRIMARY operand bare and a record is primary, so `{a: 1} ?? z`, `{a: 1} ? t
/// : o` and `{a: 1} === z` all start with `{` while being no kind of record
/// themselves. Guarding on the body's top node instead let those three through
/// as `x => {a: 1} ?? z`, which is a syntax error rather than a different
/// meaning (`codec_round_trip.rs`, F1).
///
/// Everything without that leading brace is safe bare - an arrow body extends
/// as far to the right as it can, which is exactly what any operator, call or
/// member chain inside it wants, and [`emit_operand`] is what stops the arrow
/// ITSELF swallowing an operator that follows it.
fn emit_arrow_body(out: &mut String, body: &BindingExpr) {
    if begins_with_brace(body) {
        out.push('(');
        emit_binding_expr(out, body);
        out.push(')');
    } else {
        emit_binding_expr(out, body);
    }
}

/// Whether this expression's emitted text starts with `{`.
///
/// **The recursion follows the BARE splices and stops at every parenthesised
/// one**, which is what keeps it in step with the emitter rather than
/// approximating it: an operand only contributes its own first token when
/// [`is_primary`] left it unwrapped, and a member base only when
/// [`is_bare_member_base`] did - the same two predicates the emitter itself
/// branches on, so neither can drift from the text that is actually written.
///
/// It bottoms out immediately in practice, because [`BindingExpr::Record`] is
/// the only bare-splicable form whose text opens with a brace. The recursive
/// spelling is nonetheless the honest one: it stays correct if a future primary
/// or a future operator changes that, where a one-level `matches!` would
/// quietly stop covering the vocabulary.
fn begins_with_brace(expr: &BindingExpr) -> bool {
    match expr {
        BindingExpr::Record(_) => true,
        // The three operator forms: their leading operand goes through
        // `emit_operand`.
        BindingExpr::Coalesce(operands) => operands
            .first()
            .is_some_and(|first| is_primary(first) && begins_with_brace(first)),
        BindingExpr::Cond { cond, .. } => is_primary(cond) && begins_with_brace(cond),
        BindingExpr::Eq { left, .. } => is_primary(left) && begins_with_brace(left),
        // A member chain leads with its base, through the stricter predicate.
        BindingExpr::MemberOf(base, _) => is_bare_member_base(base) && begins_with_brace(base),
        // Literal, Null, Path, Array, Call, SymbolValue, Arrow and Async each
        // open with a token of their own - a value, a keyword, a name, `[`,
        // `(` or `async`.
        _ => false,
    }
}

/// **The emitter has no precedence model, so this one is conservative on
/// purpose.**
///
/// Every other arm of [`emit_binding_expr`] splices its children in with no
/// regard for how they re-parse, which was harmless while nothing in the
/// vocabulary was an *operator*: a call, an array and a record all carry their
/// own brackets. `??` and `? :` are the first forms whose meaning depends on
/// what sits beside them, and TypeScript is unforgiving about both - `a ?? b ||
/// c` is a SYNTAX ERROR rather than a precedence question, and a ternary nested
/// in another ternary's branches re-associates without parentheses.
///
/// The rule, therefore: an operand keeps its bare spelling only if it is a
/// PRIMARY expression - one whose text cannot absorb what follows it. Everything
/// else is wrapped, including a nested `??` inside a `??`, where the parentheses
/// are redundant. Redundant parentheses cost a round-trip nothing:
/// [`crate::parse::unparen`]'s equivalent strips them before lowering, and
/// `(a ?? b) ?? c` flattens back to the same n-ary node. A MISSING pair costs a
/// re-parse that means something else.
///
/// This is a stand-in for a real precedence model, which the emitter is owed and
/// which the next operator to land should bring.
fn emit_operand(out: &mut String, expr: &BindingExpr) {
    if is_primary(expr) {
        emit_binding_expr(out, expr);
    } else {
        out.push('(');
        emit_binding_expr(out, expr);
        out.push(')');
    }
}

/// [`emit_operand`]'s stricter sibling, for the thing a member chain hangs off.
///
/// A member access binds tighter than every operator, so its base admits even
/// less: `1 .x` and `[1, 2].map(..).x` are the failure cases, and a numeric
/// literal base is a syntax error outright (`1.x` lexes the dot into the
/// number). Only a name, a call and another member chain are safe bare - which
/// covers `design().isAuthoring`, the shape this variant exists for.
fn emit_member_base(out: &mut String, expr: &BindingExpr) {
    if is_bare_member_base(expr) {
        emit_binding_expr(out, expr);
    } else {
        out.push('(');
        emit_binding_expr(out, expr);
        out.push(')');
    }
}

/// Whether [`emit_member_base`] splices this base bare - factored out because
/// [`begins_with_brace`] has to ask the SAME question the emitter branches on,
/// and a second `matches!` spelling of it is exactly the drift CLAUDE.md item 5
/// names ("a control's hit region and its drawn geometry must derive from ONE
/// shared formula").
fn is_bare_member_base(expr: &BindingExpr) -> bool {
    matches!(
        expr,
        BindingExpr::Path(_)
            | BindingExpr::Call { .. }
            | BindingExpr::SymbolValue(_)
            | BindingExpr::MemberOf(..)
    )
}

/// Whether this expression's emitted text is self-delimiting - a literal, a
/// name, a bracketed collection, a call, or a member chain ending in one.
///
/// [`BindingExpr::Arrow`] and [`BindingExpr::Async`] are deliberately NOT
/// primary: an arrow body runs to the end of the expression, so
/// `(x: unknown) => x ?? y` puts the `??` INSIDE the arrow and the operand has
/// to be wrapped before any operator may follow it.
///
/// [`BindingExpr::Call`] IS primary and stays so with an arrow inside it:
/// `xs.map((x: unknown) => x) ?? y` closes the argument list with `)` before
/// the `??`, which is exactly the open tail the retired `Map` variant did not
/// have when it spelled the same source without brackets of its own.
fn is_primary(expr: &BindingExpr) -> bool {
    matches!(
        expr,
        BindingExpr::Literal(_)
            | BindingExpr::Null
            | BindingExpr::Path(_)
            | BindingExpr::Array(_)
            | BindingExpr::Record(_)
            | BindingExpr::Call { .. }
            | BindingExpr::SymbolValue(_)
            | BindingExpr::MemberOf(..)
    )
}

fn emit_block_arrow(out: &mut String, program: &BlockArrow) {
    out.push_str("async ");
    emit_binding_params(out, &program.params);
    out.push_str(" => {");
    if !program.body.is_empty() {
        out.push('\n');
        emit_block_statements(out, &program.body, 1);
    }
    out.push('}');
}

fn emit_block_statements(out: &mut String, statements: &[BlockStmt], indent: usize) {
    for statement in statements {
        emit_indent(out, indent);
        match statement {
            BlockStmt::Let { slot, value } => {
                out.push_str("const ");
                out.push_str(slot);
                out.push_str(" = ");
                emit_binding_expr(out, value);
                out.push_str(";\n");
            }
            BlockStmt::Await { slot, awaitable } => {
                if let Some(slot) = slot {
                    out.push_str("const ");
                    out.push_str(slot);
                    out.push_str(" = ");
                } else {
                    out.push_str("await ");
                }
                if slot.is_some() {
                    out.push_str("await ");
                }
                // **[`emit_operand`], not a bare splice.** `await` takes a
                // UnaryExpression, which binds tighter than every operator in
                // this vocabulary and does not admit an arrow at all: `await a
                // ?? b` is `(await a) ?? b`, and `await x => x` is a syntax
                // error. This is the one splice inside a block that a bracket,
                // a comma or a keyword does not already delimit.
                emit_operand(out, awaitable);
                out.push_str(";\n");
            }
            BlockStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                out.push_str("if (");
                emit_binding_expr(out, condition);
                out.push_str(") {\n");
                emit_block_statements(out, then_branch, indent + 1);
                emit_indent(out, indent);
                out.push('}');
                if !else_branch.is_empty() {
                    out.push_str(" else {\n");
                    emit_block_statements(out, else_branch, indent + 1);
                    emit_indent(out, indent);
                    out.push('}');
                }
                out.push('\n');
            }
            BlockStmt::Try {
                body,
                error_slot,
                catch,
            } => {
                out.push_str("try {\n");
                emit_block_statements(out, body, indent + 1);
                emit_indent(out, indent);
                out.push_str("} catch (");
                out.push_str(error_slot);
                out.push_str(") {\n");
                emit_block_statements(out, catch, indent + 1);
                emit_indent(out, indent);
                out.push_str("}\n");
            }
            BlockStmt::Return(value) => {
                out.push_str("return ");
                emit_binding_expr(out, value);
                out.push_str(";\n");
            }
        }
    }
}

/// Emit an expression (for imported-call arguments).
fn emit_expr(out: &mut String, expr: &crate::dag::Expr) {
    use crate::dag::Expr;

    match expr {
        Expr::LitBool(b) => out.push_str(&b.to_string()),
        Expr::LitS32(n) => out.push_str(&n.to_string()),
        Expr::LitS64(n) => out.push_str(&n.to_string()),
        Expr::LitF32(n) => out.push_str(&n.to_string()),
        Expr::LitF64(n) => out.push_str(&n.to_string()),
        // The same writer the binding vocabulary uses: a call argument is
        // spliced into a JSX expression container, so it is JavaScript text and
        // takes JavaScript's escapes. (An `AttrValue::Str` is NOT - it is a JSX
        // attribute string, where a backslash is a backslash and `"` would need
        // an entity - so it keeps its own verbatim spelling above.)
        Expr::LitStr(s) => push_string_literal(out, s),
        Expr::Param(_) => {
            // Parameters only appear in handlers, which are not part of the element tree
        }
        Expr::Get { path } => {
            out.push_str(path);
        }
        Expr::Bin { op, lhs, rhs } => {
            out.push('(');
            emit_expr(out, lhs);
            out.push(' ');
            match op {
                crate::dag::BinOp::Add => out.push('+'),
                crate::dag::BinOp::Sub => out.push('-'),
                crate::dag::BinOp::Mul => out.push('*'),
                crate::dag::BinOp::Div => out.push('/'),
                crate::dag::BinOp::Eq => out.push_str("=="),
                crate::dag::BinOp::Ne => out.push_str("!="),
                crate::dag::BinOp::Lt => out.push('<'),
                crate::dag::BinOp::Le => out.push_str("<="),
                crate::dag::BinOp::Gt => out.push('>'),
                crate::dag::BinOp::Ge => out.push_str(">="),
                crate::dag::BinOp::And => out.push_str("&&"),
                crate::dag::BinOp::Or => out.push_str("||"),
            }
            out.push(' ');
            emit_expr(out, rhs);
            out.push(')');
        }
        Expr::Call { callee, args } => {
            out.push_str(callee);
            out.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(out, arg);
            }
            out.push(')');
        }
    }
}

/// Emit a type shape for type arguments.
fn emit_type_shape(out: &mut String, shape: &TypeShape) {
    match shape {
        TypeShape::Bool => out.push_str("boolean"),
        // **The sized numerics, in TypeScript** (libhbui's
        // `codec_round_trip.rs`, F6). These three used to emit `i32`, `i64` and
        // `f32` - Rust/WIT spellings that TypeScript does not have, and that
        // `type_shape` has no arm for, so each re-parsed as a dangling
        // `Named("i32")` reference to a type nobody declared.
        //
        // `S64` is the one with a keyword of its own: `bigint` is what
        // `type_shape` lowers TO `S64` (`parse.rs`), so this spelling closes the
        // round trip rather than merely being legal.
        //
        // `S32` and `F32` have no distinct spelling, because TypeScript has ONE
        // numeric type - so they widen to `number`, which is what they mean
        // there, and a re-parse of `number` is `F64`. That loss belongs to the
        // target language: neither shape has a TS source (nothing in
        // `type_shape` produces them; they arrive from a declared `FuncSig`), so
        // the choice is between valid TypeScript that widens and invalid
        // TypeScript that does not come back either.
        TypeShape::S32 => out.push_str("number"),
        TypeShape::S64 => out.push_str("bigint"),
        TypeShape::F32 => out.push_str("number"),
        TypeShape::F64 => out.push_str("number"),
        // **The unsigned pair widens too, and `U64` does NOT take `bigint`.**
        // TypeScript has no unsigned type, so neither has a spelling here - the
        // same position `S32` and `F32` are in, and they take the same answer.
        //
        // `bigint` is the tempting one for `U64` because it keeps the width,
        // and it is the wrong answer: `type_shape` lowers `bigint` to `S64`, so
        // the emitted text would re-parse SIGNED while looking like a clean
        // round trip, and a u64 past i64::MAX would come back negative. A shape
        // that widens to `F64` is a loss the reader can see coming from the
        // rule above; a shape that silently changes sign is the defect the
        // unsigned variants were added to prevent.
        TypeShape::U32 => out.push_str("number"),
        TypeShape::U64 => out.push_str("number"),
        TypeShape::String => out.push_str("string"),
        TypeShape::List(inner) => {
            out.push_str("Array<");
            emit_type_shape(out, inner);
            out.push('>');
        }
        TypeShape::Option(inner) => {
            emit_type_shape(out, inner);
            out.push_str(" | undefined");
        }
        // An inline anonymous record - `{a: number, b?: string}` - written out
        // field for field.
        //
        // It used to emit `{}` and say so ("Inline anonymous records in type
        // arguments are complex. For now, emit a placeholder"), which is a
        // DECLARED gap rather than an oversight and was pinned as one (libhbui's
        // `codec_round_trip.rs`, F5): the text still parsed, so the fields were
        // dropped silently and the re-parse was an empty record that compared
        // unequal. There is nothing complex in it - a type literal's members are
        // the same `FieldDecl`s an interface holds, they take the same two key
        // spellings as a value record's, and `?` is where the optional flag
        // goes.
        //
        // The separator is `,` rather than `;`: both are TypeScript, and this is
        // the one the record VALUE spelling beside it already uses.
        TypeShape::Record(fields) => {
            out.push('{');
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                push_property_key(out, &field.name);
                if field.optional {
                    out.push('?');
                }
                out.push_str(": ");
                emit_type_shape(out, &field.ty);
            }
            out.push('}');
        }
        TypeShape::Named(name) => out.push_str(name),
        TypeShape::Apply { constructor, args } => {
            out.push_str(constructor);
            out.push('<');
            for (index, arg) in args.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                emit_type_shape(out, arg);
            }
            out.push('>');
        }
        // The key operators re-emit as the TypeScript they were read from, so a
        // parse/emit round trip is the identity on them - through the shared
        // `key_operator_spelling`, which every other rendering surface also
        // calls, so the keys cannot come out spelled two ways.
        TypeShape::Omit { base, omitted } => emit_key_operator(out, "Omit", base, omitted),
        TypeShape::Pick { base, picked } => emit_key_operator(out, "Pick", base, picked),
        // An `extends` entry in TYPE position has no TypeScript spelling of its
        // own - the keyword belongs to the interface, not to the type. So it
        // emits as its base, which is the text that was inside the clause.
        // `emit_interface` writes the `extends` itself.
        TypeShape::Extends { base } => emit_type_shape(out, base),
    }
}

/// `Omit<Base, "a" | "b">` - this module's base rendering, the shared key
/// rendering.
fn emit_key_operator(out: &mut String, operator: &str, base: &TypeShape, keys: &[String]) {
    let mut rendered = String::new();
    emit_type_shape(&mut rendered, base);
    out.push_str(&crate::dag::key_operator_spelling(operator, &rendered, keys));
}

/// Emit indentation (spaces).
fn emit_indent(out: &mut String, level: usize) {
    for _ in 0..(level * 4) {
        out.push(' ');
    }
}

/// Emit a TypeScript interface declaration.
pub fn emit_interface(out: &mut String, iface: &InterfaceDecl) {
    out.push_str("interface ");
    out.push_str(&iface.name);
    // The clause, so a parse/emit round trip does not quietly drop it - which
    // is the exact failure `TypeShape::Extends` exists to fix, and it would
    // reappear here if only the parser learned it.
    for (index, entry) in iface.extends.iter().enumerate() {
        out.push_str(if index == 0 { " extends " } else { ", " });
        emit_type_shape(out, entry);
    }
    out.push_str(" {\n");

    for field in &iface.fields {
        out.push_str("    ");
        out.push_str(&field.name);
        if field.optional {
            out.push('?');
        }
        out.push_str(": ");
        emit_type_shape(out, &field.ty);
        out.push_str(";\n");
    }

    out.push_str("}\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{ImportName, ImportKind, Node as DagNode, PropertyAccessor};

    #[test]
    fn emit_simple_element() {
        let elem = Element {
            tag: "Button".to_string(),
            type_args: vec![],
            attrs: vec![("label".to_string(), AttrValue::Str("Click me".to_string()))],
            children: vec![DagNode::Text("Press".to_string())],
        };

        let doc = TsxDocument {
            root_nodes: vec![DagNode::Element(elem)],
            imports: vec![],
        };

        let output = emit_tsx_document(&doc);
        assert!(output.contains("<Button label=\"Click me\">"));
        assert!(output.contains("Press"));
        assert!(output.contains("</Button>"));
    }

    #[test]
    fn emit_self_closing_element() {
        let elem = Element {
            tag: "Icon".to_string(),
            type_args: vec![],
            attrs: vec![("name".to_string(), AttrValue::Str("star".to_string()))],
            children: vec![],
        };

        let doc = TsxDocument {
            root_nodes: vec![DagNode::Element(elem)],
            imports: vec![],
        };

        let output = emit_tsx_document(&doc);
        assert!(output.contains("<Icon name=\"star\" />"));
    }

    #[test]
    fn emit_element_with_type_args() {
        let elem = Element {
            tag: "List".to_string(),
            type_args: vec![TypeShape::Named("Message".to_string())],
            attrs: vec![("value".to_string(), AttrValue::Binding("items".to_string()))],
            children: vec![],
        };

        let doc = TsxDocument {
            root_nodes: vec![DagNode::Element(elem)],
            imports: vec![],
        };

        let output = emit_tsx_document(&doc);
        assert!(output.contains("<List<Message>"));
    }

    #[test]
    fn emit_nested_generic_type_and_owned_binding_expression() {
        let elem = Element {
            tag: "ResultView".into(),
            type_args: vec![TypeShape::Apply {
                constructor: "Result".into(),
                args: vec![
                    TypeShape::Named("User".into()),
                    TypeShape::Named("Error".into()),
                ],
            }],
            attrs: vec![(
                "value".into(),
                AttrValue::BindingExpr(BindingExpr::Record(vec![
                    (
                        "rows".into(),
                        BindingExpr::Array(vec![BindingExpr::Literal(LiteralValue::Int64(3))]),
                    ),
                    (
                        "load".into(),
                        BindingExpr::Call {
                            namespace: "storage".into(),
                            name: "load".into(),
                            type_args: vec![TypeShape::Named("User".into())],
                            args: vec![BindingExpr::Path(vec!["props".into(), "id".into()])],
                        },
                    ),
                ])),
            )],
            children: vec![],
        };
        let doc = TsxDocument {
            root_nodes: vec![DagNode::Element(elem)],
            imports: vec![],
        };
        let output = emit_tsx_document(&doc);
        assert!(output.contains("<ResultView<Result<User, Error>>"));
        assert!(output.contains("value={{rows: [3], load: storage.load<User>(props.id)}}"));
    }

    #[test]
    fn emit_import_declarations() {
        let doc = TsxDocument {
            root_nodes: vec![],
            imports: vec![
                ImportDecl {
                    source: "Library".to_string(),
                    names: vec![
                        ImportName {
                            local: "lib".to_string(),
                            imported: "default".to_string(),
                            kind: ImportKind::Default,
                        },
                    ],
                },
                ImportDecl {
                    source: "host:effects".to_string(),
                    names: vec![
                        ImportName {
                            local: "navigate".to_string(),
                            imported: "navigate".to_string(),
                            kind: ImportKind::Named,
                        },
                        ImportName {
                            local: "tap".to_string(),
                            imported: "onTap".to_string(),
                            kind: ImportKind::Named,
                        },
                    ],
                },
            ],
        };

        let output = emit_tsx_document(&doc);
        assert!(output.contains("import { default as lib } from \"Library\";"));
        assert!(output.contains("import { navigate, onTap as tap } from \"host:effects\";"));
    }

    #[test]
    fn emit_boolean_attributes() {
        let elem = Element {
            tag: "Item".to_string(),
            type_args: vec![],
            attrs: vec![
                ("visible".to_string(), AttrValue::Bool(true)),
                ("disabled".to_string(), AttrValue::Bool(false)),
            ],
            children: vec![],
        };

        let doc = TsxDocument {
            root_nodes: vec![DagNode::Element(elem)],
            imports: vec![],
        };

        let output = emit_tsx_document(&doc);
        assert!(output.contains("visible={true}"));
        assert!(output.contains("disabled={false}"));
    }

    #[test]
    fn emit_numeric_attributes() {
        let elem = Element {
            tag: "Box".to_string(),
            type_args: vec![],
            attrs: vec![
                ("width".to_string(), AttrValue::Num(100.0)),
                ("height".to_string(), AttrValue::Num(50.5)),
            ],
            children: vec![],
        };

        let doc = TsxDocument {
            root_nodes: vec![DagNode::Element(elem)],
            imports: vec![],
        };

        let output = emit_tsx_document(&doc);
        assert!(output.contains("width={100}"));
        assert!(output.contains("height={50.5}"));
    }
    // --- F1: an arrow body is guarded on its leading TOKEN -------------------
    //
    // These assert the emitted TEXT, with the known-bad twin beside each case:
    // a body whose leading operand is NOT a record must stay unparenthesised,
    // or "fixed" would just mean "parenthesises everything".

    /// `{a: 1}`, the operand the hazard is about.
    fn record() -> BindingExpr {
        BindingExpr::Record(vec![(
            "a".into(),
            BindingExpr::Literal(LiteralValue::Int64(1)),
        )])
    }

    /// `z`.
    fn name(text: &str) -> BindingExpr {
        BindingExpr::Path(vec![text.into()])
    }

    /// `(x: number) => <body>`.
    fn arrow(body: BindingExpr) -> BindingExpr {
        BindingExpr::Arrow {
            params: vec![BindingParam {
                name: "x".into(),
                ty: TypeShape::F64,
            }],
            body: Box::new(body),
        }
    }

    #[test]
    fn an_arrow_body_whose_leading_operand_is_a_record_is_parenthesised() {
        let cases = [
            (
                BindingExpr::Coalesce(vec![record(), name("z")]),
                "(x: number) => ({a: 1} ?? z)",
            ),
            (
                BindingExpr::Cond {
                    cond: Box::new(record()),
                    then: Box::new(name("t")),
                    other: Box::new(name("o")),
                },
                "(x: number) => ({a: 1} ? t : o)",
            ),
            (
                BindingExpr::Eq {
                    left: Box::new(record()),
                    right: Box::new(name("z")),
                    strict: true,
                },
                "(x: number) => ({a: 1} === z)",
            ),
            // The body that IS a record - the case the old guard covered, which
            // must keep working.
            (record(), "(x: number) => ({a: 1})"),
        ];
        for (body, expected) in cases {
            assert_eq!(String::from(&arrow(body)), expected);
        }
    }

    #[test]
    fn an_arrow_body_without_a_leading_brace_stays_bare() {
        let cases = [
            // The known-bad twin of each case above: same operator, leading
            // operand that is not a record.
            (
                BindingExpr::Coalesce(vec![name("a"), name("z")]),
                "(x: number) => a ?? z",
            ),
            (
                BindingExpr::Cond {
                    cond: Box::new(name("c")),
                    then: Box::new(name("t")),
                    other: Box::new(name("o")),
                },
                "(x: number) => c ? t : o",
            ),
            (
                BindingExpr::Eq {
                    left: Box::new(name("a")),
                    right: Box::new(name("z")),
                    strict: true,
                },
                "(x: number) => a === z",
            ),
            // An ARRAY leads with `[`, which is not the hazard.
            (
                BindingExpr::Coalesce(vec![
                    BindingExpr::Array(vec![BindingExpr::Literal(LiteralValue::Int64(1))]),
                    name("z"),
                ]),
                "(x: number) => [1] ?? z",
            ),
            // A member chain on a record: `emit_member_base` already wrapped the
            // record, so the text leads with `(` and needs nothing more.
            (
                BindingExpr::MemberOf(Box::new(record()), PropertyAccessor::Named("m".into())),
                "(x: number) => ({a: 1}).m",
            ),
            // And one level in: a coalesce LED BY that member chain.
            (
                BindingExpr::Coalesce(vec![
                    BindingExpr::MemberOf(
                        Box::new(record()),
                        PropertyAccessor::Named("m".into()),
                    ),
                    name("z"),
                ]),
                "(x: number) => ({a: 1}).m ?? z",
            ),
            // A record in the TRAILING position is not the hazard either.
            (
                BindingExpr::Coalesce(vec![name("z"), record()]),
                "(x: number) => z ?? {a: 1}",
            ),
        ];
        for (body, expected) in cases {
            assert_eq!(String::from(&arrow(body)), expected);
        }
    }

    #[test]
    fn the_guard_reaches_through_a_nested_arrow() {
        // `(x: number) => ({a: 1}) ?? ((x: number) => ({a: 1}) ?? z)` - the
        // recursive case in the matrix. The inner arrow is an operand of the
        // outer coalesce, so `emit_operand` wraps it (an arrow is not primary),
        // and each arrow's own body is guarded independently.
        let inner = arrow(BindingExpr::Coalesce(vec![record(), name("z")]));
        let outer = arrow(BindingExpr::Coalesce(vec![record(), inner]));
        assert_eq!(
            String::from(&outer),
            "(x: number) => ({a: 1} ?? ((x: number) => ({a: 1} ?? z)))"
        );
    }

    // --- F3: a string literal is escaped -------------------------------------
    //
    // Asserted from a HAND-BUILT node, which is the direction the seam test
    // beside it cannot reach: a dag that never came from a parse is exactly the
    // one an editor hands the emitter.

    /// `"<value>"` as the emitter writes it.
    fn literal(value: &str) -> String {
        String::from(&BindingExpr::Literal(LiteralValue::String(value.into())))
    }

    #[test]
    fn a_string_literal_carries_its_escapes() {
        assert_eq!(literal("back\\slash"), r#""back\\slash""#);
        assert_eq!(literal("quote\"inside"), r#""quote\"inside""#);
        assert_eq!(literal("line\nbreak"), r#""line\nbreak""#);
        assert_eq!(literal("carriage\rreturn"), r#""carriage\rreturn""#);
        assert_eq!(literal("tab\there"), r#""tab\there""#);
        assert_eq!(literal("\u{1}"), "\"\\u0001\"");
        assert_eq!(literal("\u{2028}"), "\"\\u2028\"");
    }

    // --- F4: a record key is quoted when it cannot be written bare -----------

    /// `{<key>: 1}` as the emitter writes it.
    fn keyed(key: &str) -> String {
        String::from(&BindingExpr::Record(vec![(
            key.into(),
            BindingExpr::Literal(LiteralValue::Int64(1)),
        )]))
    }

    #[test]
    fn a_record_key_that_is_not_an_identifier_is_quoted() {
        assert_eq!(keyed("quoted-key"), r#"{"quoted-key": 1}"#);
        assert_eq!(keyed("with space"), r#"{"with space": 1}"#);
        assert_eq!(keyed("0leading"), r#"{"0leading": 1}"#);
        assert_eq!(keyed(""), r#"{"": 1}"#);
        // The key goes through the same escaping the values do.
        assert_eq!(keyed("quote\"in"), r#"{"quote\"in": 1}"#);
    }

    // --- F5: an inline record type is written field for field ----------------

    #[test]
    fn an_inline_record_type_is_written_field_for_field() {
        use crate::dag::FieldDecl;
        let shape = TypeShape::Record(vec![
            FieldDecl {
                name: "a".into(),
                ty: TypeShape::F64,
                optional: false,
            },
            FieldDecl {
                name: "b-c".into(),
                ty: TypeShape::Option(Box::new(TypeShape::String)),
                optional: true,
            },
        ]);
        let mut out = String::new();
        emit_type_shape(&mut out, &shape);
        assert_eq!(out, "{a: number, \"b-c\"?: string | undefined}");

        // An empty one still writes the two braces it always did.
        let mut out = String::new();
        emit_type_shape(&mut out, &TypeShape::Record(vec![]));
        assert_eq!(out, "{}");
    }

    // --- F6: the sized numerics are written in TypeScript --------------------

    /// One type shape's text.
    fn type_text(shape: &TypeShape) -> String {
        let mut out = String::new();
        emit_type_shape(&mut out, shape);
        out
    }

    #[test]
    fn the_sized_numerics_are_written_in_typescript() {
        // `bigint` is a keyword, and the one `type_shape` lowers to `S64`, so
        // this spelling closes the round trip.
        assert_eq!(type_text(&TypeShape::S64), "bigint");
        // The other two widen: TypeScript has one numeric type, and `number` is
        // what they mean in it. Stated as a test rather than left implicit,
        // because the loss is real and it is the target language's, not this
        // emitter's.
        assert_eq!(type_text(&TypeShape::S32), "number");
        assert_eq!(type_text(&TypeShape::F32), "number");
        // Unchanged, and here so the whole vocabulary is written down in one
        // place: these three spellings were TypeScript's already.
        assert_eq!(type_text(&TypeShape::Bool), "boolean");
        assert_eq!(type_text(&TypeShape::F64), "number");
        assert_eq!(type_text(&TypeShape::String), "string");
    }

    #[test]
    fn an_identifier_record_key_stays_bare() {
        // Quoting everything would round-trip too, and would respell every
        // record in the corpus on the next publish.
        for key in ["a", "camelCase", "_under", "$dollar", "a0"] {
            assert_eq!(keyed(key), format!("{{{key}: 1}}"));
        }
    }

    #[test]
    fn a_string_literal_with_nothing_to_escape_is_written_as_it_stands() {
        // The over-correction guard: "escape everything" would pass the test
        // above and be a different bug.
        assert_eq!(literal("plain text 1.0"), r#""plain text 1.0""#);
        assert_eq!(literal("it's $5 (100%) - `ok`"), r#""it's $5 (100%) - `ok`""#);
        assert_eq!(literal("caf\u{e9}"), "\"caf\u{e9}\"");
        assert_eq!(literal(""), r#""""#);
    }
}

#[cfg(all(test, feature = "parse"))]
mod roundtrip_tests {
    use super::*;
    use crate::dag::{FuncSig, FieldDecl, TypeShape, Resolution, ParserHost};
    use crate::parse::ParseCtx;

    /// Mock host that grants common effects used in test fixtures.
    struct TestHost;

    impl ParserHost for TestHost {
        fn resolve(&self, specifier: &str) -> Option<Resolution<'_>> {
            if specifier == "host:effects" {
                // Build effect signatures on the fly
                let sigs = vec![
                    FuncSig {
                        name: "navigate".into(),
                        params: vec![FieldDecl {
                            name: "screen".into(),
                            ty: TypeShape::String,
                            optional: false,
                        }],
                        result: None,
                    },
                    FuncSig {
                        name: "toggleDrawer".into(),
                        params: vec![],
                        result: None,
                    },
                ];

                // Convert to static lifetime - this is a hack for testing
                // In real code, you'd use LazyLock or similar
                let leaked: &'static [FuncSig] = Box::leak(sigs.into_boxed_slice());
                Some(Resolution::Host(leaked))
            } else {
                None
            }
        }
    }

    /// How many comment nodes a document carries, at every depth.
    fn comments_in(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| match n {
                Node::Comment(_) => 1,
                Node::Element(e) => comments_in(&e.children),
                Node::Text(_) | Node::Expr(_) => 0,
            })
            .sum()
    }

    /// How many comments the SOURCE has, counted without the parser.
    ///
    /// An independent measure on purpose: "the retained count equals what the
    /// parse retained" is not a claim about anything. Every fixture in the
    /// corpus comments with `//` at the head of a line, so a line scan is a
    /// second opinion the parser cannot influence - and if a fixture ever grows
    /// a block comment or a `//` inside a string, this disagrees loudly rather
    /// than quietly measuring the wrong thing.
    fn line_comments(source: &str) -> usize {
        source
            .lines()
            .filter(|l| l.trim_start().starts_with("//"))
            .count()
    }

    /// The round trip, in BOTH parses of the same source: the one that retains
    /// comments and the one that does not.
    ///
    /// Both, every time, because they are the two production paths and they
    /// must not diverge in anything but comments: the publish path parses
    /// without them (so no `.hbdef` can carry one) and the editor path parses
    /// with them (so the source it shows is the source that was written).
    fn roundtrip_test(tsx_source: &str, test_name: &str) {
        roundtrip_once(tsx_source, test_name, false);

        let retained = roundtrip_once(tsx_source, test_name, true);
        let authored = line_comments(tsx_source);
        assert_eq!(
            retained, authored,
            "{test_name}: the source has {authored} comment lines and the retaining parse kept {retained}"
        );
    }

    /// One round trip, in one context. Answers how many comments survived it.
    fn roundtrip_once(tsx_source: &str, test_name: &str, retain: bool) -> usize {
        eprintln!("Testing roundtrip for: {} (retain_comments={retain})", test_name);

        // Create a context with a host that grants effects
        let builder = ParseCtx::builder().set_host(TestHost);
        let ctx = if retain {
            builder.retain_comments().build()
        } else {
            builder.build()
        };

        // Parse the original source
        let doc1 = match ctx.parse_tsx(tsx_source) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("Failed to parse original: {}", e);
                panic!("Failed to parse {}: {}", test_name, e);
            }
        };

        // Emit it back to source
        let emitted = emit_tsx_document(&doc1);
        eprintln!("Emitted source length: {}", emitted.len());

        // Parse the emitted source using the same context
        let doc2 = match ctx.parse_tsx(&emitted) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("Failed to parse emitted: {}", e);
                eprintln!("Emitted source:\n{}", emitted);
                panic!("Failed to parse emitted TSX for {}: {}", test_name, e);
            }
        };

        // Assert the two DAGs are equal
        if doc1 != doc2 {
            eprintln!("DAGs differ for {}", test_name);

            // Compare imports
            if doc1.imports != doc2.imports {
                eprintln!("Imports differ:");
                eprintln!("  Original: {:#?}", doc1.imports);
                eprintln!("  Emitted: {:#?}", doc2.imports);
            }

            // Compare root nodes
            if doc1.root_nodes != doc2.root_nodes {
                eprintln!("Root nodes differ:");
                eprintln!("  Original count: {}", doc1.root_nodes.len());
                eprintln!("  Emitted count: {}", doc2.root_nodes.len());

                // Check first node in detail if they have different structure
                if !doc1.root_nodes.is_empty() && !doc2.root_nodes.is_empty() {
                    if doc1.root_nodes[0] != doc2.root_nodes[0] {
                        eprintln!("  First root node differs");
                        // Don't print the full structure as it's too verbose
                    }
                }
            }

            panic!("Round-trip failed for {}: DAGs not equal", test_name);
        }

        let comments = comments_in(&doc1.root_nodes);
        if !retain {
            // The publish guarantee, asserted rather than assumed: a parse that
            // was not asked for comments constructs the variant nowhere, so
            // nothing downstream of it needs a filter.
            assert_eq!(
                comments, 0,
                "{test_name}: a parse without retain_comments produced {comments} comment nodes"
            );
        }

        eprintln!("Round-trip successful for: {}", test_name);
        comments
    }

    #[test]
    fn roundtrip_baychat_App() {
        let source = include_str!("../tests/fixtures/baychat_App.tsx");
        roundtrip_test(source, "baychat/App.tsx");
    }

    #[test]
    fn roundtrip_baychat_chat() {
        let source = include_str!("../tests/fixtures/baychat_chat.tsx");
        roundtrip_test(source, "baychat/screens/chat.tsx");
    }

    #[test]
    fn roundtrip_baychat_profile() {
        let source = include_str!("../tests/fixtures/baychat_profile.tsx");
        roundtrip_test(source, "baychat/screens/profile.tsx");
    }

    #[test]
    fn roundtrip_baychat_chat_feed_entry() {
        let source = include_str!("../tests/fixtures/baychat_chat_feed_entry.tsx");
        roundtrip_test(source, "baychat/widgets/chat_feed_entry.tsx");
    }

    #[test]
    fn roundtrip_baychat_message_input() {
        let source = include_str!("../tests/fixtures/baychat_message_input.tsx");
        roundtrip_test(source, "baychat/widgets/message_input.tsx");
    }

    #[test]
    fn roundtrip_default_App() {
        let source = include_str!("../tests/fixtures/default_App.tsx");
        roundtrip_test(source, "default/App.tsx");
    }

    #[test]
    fn roundtrip_default_home() {
        let source = include_str!("../tests/fixtures/default_home.tsx");
        roundtrip_test(source, "default/screens/home.tsx");
    }

    #[test]
    fn roundtrip_libhbui_chat() {
        let source = include_str!("../tests/fixtures/libhbui_chat.tsx");
        roundtrip_test(source, "crates/libhbui/fixtures/chat.tsx");
    }

    // --- the shapes the corpus does not have ---------------------------------
    //
    // Every fixture above comments the same way: `//` at the head of a line,
    // above the code. That is the corpus Highbay actually writes, and it would
    // leave three shapes untested - a comment among JSX CHILDREN, a comment
    // between two elements, and a BLOCK comment - each of which the emitter
    // punctuates differently and any of which a future source may use.

    /// The retaining context, spelled once.
    fn retaining() -> ParseCtx {
        ParseCtx::builder().set_host(TestHost).retain_comments().build()
    }

    /// The comments of a document, in order, at every depth.
    fn comment_texts(nodes: &[Node]) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(nodes: &[Node], out: &mut Vec<String>) {
            for n in nodes {
                match n {
                    Node::Comment(t) => out.push(t.clone()),
                    Node::Element(e) => walk(&e.children, out),
                    Node::Text(_) | Node::Expr(_) => {}
                }
            }
        }
        walk(nodes, &mut out);
        out
    }

    #[test]
    fn a_block_comment_among_jsx_children_survives_the_round_trip() {
        // `{/* ... */}` is the only way JSX can hold a comment among children:
        // bare `// note` there is TEXT, and would draw.
        const SOURCE: &str = r#"
<Screen>
    {/* above the column */}
    <Column>
        <Item />
        {/* between two items */}
        <Item />
    </Column>
</Screen>
"#;
        let ctx = retaining();
        let doc = ctx.parse_tsx(SOURCE).expect("parse");
        assert_eq!(
            comment_texts(&doc.root_nodes),
            vec![
                "/* above the column */",
                "/* between two items */",
            ],
            "both child comments are retained, verbatim and in authored order"
        );

        let emitted = emit_tsx_document(&doc);
        let back = ctx.parse_tsx(&emitted).unwrap_or_else(|e| {
            panic!("re-parsing the emitted source failed: {e}\n{emitted}")
        });
        assert_eq!(doc, back, "emitted source:\n{emitted}");
    }

    #[test]
    fn a_line_comment_among_jsx_children_survives_the_round_trip() {
        // A `//` comment inside a JSX expression container is legal, and it is
        // the one shape where the emitter cannot just wrap the text in braces:
        // `{// note}` comments out its own closing brace, so the brace has to
        // go on the next line. This is that case, end to end.
        const SOURCE: &str = "
<Screen>
    {// a line comment, in braces
    }
    <Column />
</Screen>
";
        let ctx = retaining();
        let doc = ctx.parse_tsx(SOURCE).expect("parse");
        assert_eq!(
            comment_texts(&doc.root_nodes),
            vec!["// a line comment, in braces"]
        );

        let emitted = emit_tsx_document(&doc);
        assert!(
            !emitted.contains("// a line comment, in braces}"),
            "the closing brace must not be inside the line comment:\n{emitted}"
        );
        let back = ctx.parse_tsx(&emitted).unwrap_or_else(|e| {
            panic!("re-parsing the emitted source failed: {e}\n{emitted}")
        });
        assert_eq!(doc, back, "emitted source:\n{emitted}");
    }

    #[test]
    fn a_comment_between_two_root_statements_keeps_its_place() {
        const SOURCE: &str = r#"
// above the import
import { navigate } from "host:effects";
// below the import, above the screen
<Screen />
"#;
        let ctx = retaining();
        let doc = ctx.parse_tsx(SOURCE).expect("parse");
        assert_eq!(
            comment_texts(&doc.root_nodes),
            vec!["// above the import", "// below the import, above the screen"],
        );
        // Both are BEFORE the element, because both were written before it -
        // the import is not a root node, so it does not separate them.
        assert!(
            matches!(doc.root_nodes.last(), Some(Node::Element(e)) if e.tag == "Screen"),
            "the element is still the last root: {:?}",
            doc.root_nodes
        );

        let emitted = emit_tsx_document(&doc);
        let back = ctx.parse_tsx(&emitted).expect("re-parse");
        assert_eq!(doc, back, "emitted source:\n{emitted}");
    }

    #[test]
    fn without_retention_the_same_sources_carry_no_comment_at_all() {
        // The publish guarantee. Not "the comments are filtered out
        // downstream" - the variant is never constructed, so there is no
        // downstream filter to forget.
        const SOURCES: [&str; 2] = [
            "// a header\n<Screen>\n{/* a child */}\n<Column />\n</Screen>\n",
            "// only a header\n<Screen />\n",
        ];
        let ctx = ParseCtx::builder().set_host(TestHost).build();
        for source in SOURCES {
            let doc = ctx.parse_tsx(source).expect("parse");
            assert_eq!(
                comment_texts(&doc.root_nodes),
                Vec::<String>::new(),
                "the default context retained a comment from:\n{source}"
            );
        }
        // And the free function, which is that same default context.
        let doc = crate::parse_tsx(SOURCES[0]).expect("parse");
        assert_eq!(comment_texts(&doc.root_nodes), Vec::<String>::new());
    }

}
