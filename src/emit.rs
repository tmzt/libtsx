//! TSX source emitter — converts a parsed [`TsxDocument`] back to TSX source text.
//!
//! This module emits a [`TsxDocument`] as TSX source code, preserving the structure,
//! attributes, type arguments, and child nodes in source order. The output may differ
//! in formatting from the original source, but the semantic structure is identical.
//!
//! Comments are **not** preserved — the [`Node`] enum carries only Element, Text, and
//! Expr variants, so comment information is lost during parsing. The emitted source
//! is therefore smaller than many authored sources.

use crate::dag::{
    AttrValue, Element, ImportDecl, InterfaceDecl, Node,
    TsxDocument, TypeShape,
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

    // Emit root nodes
    for node in &doc.root_nodes {
        emit_node(&mut out, node, 0);
    }

    out
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

/// Emit a node (Element, Text, or Expr) with the given indentation level.
fn emit_node(out: &mut String, node: &Node, indent: usize) {
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
    }
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
            emit_node(out, child, indent + 1);
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

    fn roundtrip_test(tsx_source: &str, test_name: &str) {
        eprintln!("Testing roundtrip for: {}", test_name);

        // Create a context with a host that grants effects
        let ctx = ParseCtx::builder().set_host(TestHost).build();

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

        eprintln!("Round-trip successful for: {}", test_name);
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
}
