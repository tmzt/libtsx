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
    AttrValue, BindingExpr, BindingLiteral, EffectProgram, EffectStmt, Element, ImportDecl,
    InterfaceDecl, Node, TsxDocument, TypeShape,
};

/// Emit a [`TsxDocument`] back to TSX source text.
///
/// The output preserves the semantic structure of the document: imports in order,
/// interfaces in order, and the element tree with all attributes and children in
/// their authored order. Type arguments, effect bindings, and all attribute value
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
        AttrValue::NamedEffect(effect) => {
            out.push_str("={");
            // Emit just the effect name, not the namespace.
            // The namespace is established by the import and should not appear
            // in the JSX expression (e.g., emit "navigate(...)" not "host:effects.navigate(...)")
            out.push_str(&effect.name);
            out.push('(');

            for (i, arg) in effect.args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(out, arg);
            }
            out.push_str(")}");
        }
    }
}

/// Emit one owned object-binding expression. Unlike [`AttrValue::NamedEffect`],
/// this vocabulary is not used by the legacy event parser; it is emitted only
/// when a caller has already constructed the owned semantic IR.
fn emit_binding_expr(out: &mut String, expr: &BindingExpr) {
    match expr {
        BindingExpr::Literal(literal) => match literal {
            BindingLiteral::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            BindingLiteral::Number(value) => out.push_str(&value.to_string()),
            BindingLiteral::String(value) => {
                out.push('"');
                out.push_str(value);
                out.push('"');
            }
            BindingLiteral::Null => out.push_str("null"),
        },
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
                out.push_str(name);
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
        BindingExpr::Map {
            source,
            param,
            body,
        } => {
            emit_binding_expr(out, source);
            out.push_str(".map(");
            out.push_str(param);
            out.push_str(" => ");
            emit_binding_expr(out, body);
            out.push(')');
        }
        BindingExpr::Async(program) => emit_effect_program(out, program),
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
        BindingExpr::Member { base, path } => {
            emit_member_base(out, base);
            for segment in path {
                out.push('.');
                out.push_str(segment);
            }
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
    if matches!(
        expr,
        BindingExpr::Path(_) | BindingExpr::Call { .. } | BindingExpr::Member { .. }
    ) {
        emit_binding_expr(out, expr);
    } else {
        out.push('(');
        emit_binding_expr(out, expr);
        out.push(')');
    }
}

/// Whether this expression's emitted text is self-delimiting - a literal, a
/// name, a bracketed collection, a call, or a member chain ending in one.
///
/// [`BindingExpr::Map`] is deliberately NOT primary despite ending in `)`: it
/// emits `source.map(p => body)` and the arrow body runs to the end of the
/// expression, so `xs.map(x => x) ?? y` re-parses with the `??` INSIDE the
/// arrow. [`BindingExpr::Async`] has the same open tail.
fn is_primary(expr: &BindingExpr) -> bool {
    matches!(
        expr,
        BindingExpr::Literal(_)
            | BindingExpr::Path(_)
            | BindingExpr::Array(_)
            | BindingExpr::Record(_)
            | BindingExpr::Call { .. }
            | BindingExpr::Member { .. }
    )
}

fn emit_effect_program(out: &mut String, program: &EffectProgram) {
    out.push_str("async (");
    for (index, param) in program.params.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&param.name);
        out.push_str(": ");
        emit_type_shape(out, &param.ty);
    }
    out.push_str(") => {");
    if !program.body.is_empty() {
        out.push('\n');
        emit_effect_statements(out, &program.body, 1);
    }
    out.push('}');
}

fn emit_effect_statements(out: &mut String, statements: &[EffectStmt], indent: usize) {
    for statement in statements {
        emit_indent(out, indent);
        match statement {
            EffectStmt::Let { slot, value } => {
                out.push_str("const ");
                out.push_str(slot);
                out.push_str(" = ");
                emit_binding_expr(out, value);
                out.push_str(";\n");
            }
            EffectStmt::Await { slot, awaitable } => {
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
                emit_binding_expr(out, awaitable);
                out.push_str(";\n");
            }
            EffectStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                out.push_str("if (");
                emit_binding_expr(out, condition);
                out.push_str(") {\n");
                emit_effect_statements(out, then_branch, indent + 1);
                emit_indent(out, indent);
                out.push('}');
                if !else_branch.is_empty() {
                    out.push_str(" else {\n");
                    emit_effect_statements(out, else_branch, indent + 1);
                    emit_indent(out, indent);
                    out.push('}');
                }
                out.push('\n');
            }
            EffectStmt::Try {
                body,
                error_slot,
                catch,
            } => {
                out.push_str("try {\n");
                emit_effect_statements(out, body, indent + 1);
                emit_indent(out, indent);
                out.push_str("} catch (");
                out.push_str(error_slot);
                out.push_str(") {\n");
                emit_effect_statements(out, catch, indent + 1);
                emit_indent(out, indent);
                out.push_str("}\n");
            }
            EffectStmt::Return(value) => {
                out.push_str("return ");
                emit_binding_expr(out, value);
                out.push_str(";\n");
            }
        }
    }
}

/// Emit an expression (for effect arguments).
fn emit_expr(out: &mut String, expr: &crate::dag::Expr) {
    use crate::dag::Expr;

    match expr {
        Expr::LitBool(b) => out.push_str(&b.to_string()),
        Expr::LitS32(n) => out.push_str(&n.to_string()),
        Expr::LitS64(n) => out.push_str(&n.to_string()),
        Expr::LitF32(n) => out.push_str(&n.to_string()),
        Expr::LitF64(n) => out.push_str(&n.to_string()),
        Expr::LitStr(s) => {
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
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
        TypeShape::S32 => out.push_str("i32"),
        TypeShape::S64 => out.push_str("i64"),
        TypeShape::F32 => out.push_str("f32"),
        TypeShape::F64 => out.push_str("number"),
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
        TypeShape::Record(_) => {
            // Inline anonymous records in type arguments are complex
            // For now, emit a placeholder
            out.push_str("{}");
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
    }
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
    use crate::dag::{ImportName, ImportKind, Node as DagNode};

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
                        BindingExpr::Array(vec![BindingExpr::Literal(BindingLiteral::Number(
                            3.0,
                        ))]),
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
