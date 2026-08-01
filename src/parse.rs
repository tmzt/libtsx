//! The oxc-backed TSX parser (feature `parse`).
//!
//! Lowers TSX source into a fully-owned, oxc-free tree
//! ([`crate::dag::TsxDocument`]) plus extracts TypeScript `interface`
//! declarations into the owned [`crate::dag`] code-graph types
//! ([`extract_interfaces`]).
//!
//! Design constraints (PLAN.md §4, Phase 7):
//! * **No `oxc_*` type appears in this module's public API** — callers get
//!   plain owned Rust data.
//! * **This module owns no data types.** Everything it produces —
//!   [`Element`], [`Node`], [`AttrValue`], [`TsxDocument`], [`InterfaceDecl`],
//!   [`ImportDecl`] — is defined in [`crate::dag`] and is available without
//!   the `parse` feature. The parser is a *producer* of graph values, not the
//!   home of any.
//! * **Deterministic output** — JSX attributes keep source order (a `Vec`,
//!   not a `HashMap`) so downstream node-graph serialization is stable.
//! * Expression children (`{binding}`) and string-literal children are
//!   captured (the old proof-of-concept dropped them).

use crate::dag::{
    AttrValue, Element, FieldDecl, ImportDecl, ImportKind, ImportName, InterfaceDecl, Node,
    TsxDocument, TypeShape,
};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, ExportDefaultDeclarationKind, Expression, ImportDeclarationSpecifier,
    JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement, JSXElementName,
    ModuleExportName, PropertyKey, Statement, TSSignature, TSType,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// Parse TSX source into an owned [`TsxDocument`].
///
/// Two module shapes contribute root JSX nodes (EDITOR_PLAN §7 / MODULE_PLAN):
/// * **bare JSX** — a top-level `<JSX/>` expression statement (the original
///   Highbay screen/widget shape); every such statement contributes a root node.
/// * **export-default arrow component** — `export default () => (<JSX/>)`, or a
///   `const Name = () => (<JSX/>)` referenced by `export default Name`. The
///   arrow's returned JSX element becomes the (single) root node. This is the
///   full-module screen shape: an `import` for a provider plus a default-exported
///   component that passes it as a prop.
///
/// Top-level `import` declarations are captured into [`TsxDocument::imports`]
/// regardless of shape. Interface and other declarations are ignored here (see
/// [`extract_interfaces`]). `export default const …` is intentionally *not*
/// accepted — it is invalid TS and oxc rejects it (use the `const … ;
/// export default …` split, which is what Highbay writes).
pub fn parse_tsx(source: &str) -> Result<TsxDocument, Vec<String>> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::tsx()).parse();

    if !ret.diagnostics.is_empty() {
        return Err(ret
            .diagnostics
            .into_iter()
            // `{e}`, not `{e:?}`. These strings are USER-FACING -- the IDE's
            // live-parse status strip renders them verbatim -- and the Debug
            // form spells the whole struct, so a typo appeared in the editor as
            // `Parse error: OxcDiagnostic { inner: OxcDiagnosticInner {
            // message: "Unexpected token", l...`, truncated mid-field. Display
            // is the rendered diagnostic oxc means a human to read.
            .map(|e| e.to_string())
            .collect());
    }

    let mut root_nodes = Vec::new();
    let mut imports = Vec::new();
    // Pass 1: bare JSX statements, imports, and a table of
    // `const Name = () => (<JSX/>)` arrow components (for export-default-by-name).
    let mut arrow_components: Vec<(&str, &JSXElement)> = Vec::new();
    for stmt in &ret.program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => imports.push(convert_import(decl)),
            Statement::ExpressionStatement(expr_stmt) => match &expr_stmt.expression {
                Expression::JSXElement(jsx) => root_nodes.push(Node::Element(convert_element(jsx))),
                Expression::JSXFragment(frag) => {
                    for child in &frag.children {
                        push_child(&mut root_nodes, child);
                    }
                }
                _ => {}
            },
            Statement::VariableDeclaration(var) => {
                for d in &var.declarations {
                    if let (Some(name), Some(Expression::ArrowFunctionExpression(arrow))) =
                        (d.id.get_binding_identifier(), d.init.as_ref())
                    {
                        if let Some(jsx) = arrow_root_jsx(arrow) {
                            arrow_components.push((name.name.as_str(), jsx));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Pass 2: the export-default component — an inline arrow, or a reference to a
    // `const` arrow component collected above. Its JSX is the module's root.
    for stmt in &ret.program.body {
        if let Statement::ExportDefaultDeclaration(decl) = stmt {
            let jsx = match &decl.declaration {
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => arrow_root_jsx(arrow),
                ExportDefaultDeclarationKind::Identifier(id) => arrow_components
                    .iter()
                    .find(|(n, _)| *n == id.name.as_str())
                    .map(|(_, jsx)| *jsx),
                _ => None,
            };
            if let Some(jsx) = jsx {
                root_nodes.push(Node::Element(convert_element(jsx)));
            }
        }
    }

    // Lenient fallback: a module with a single `const` arrow component and no
    // export/bare-JSX still yields its JSX (so a mid-edit missing `export default`
    // doesn't blank the preview).
    if root_nodes.is_empty() {
        if let Some((_, jsx)) = arrow_components.first() {
            root_nodes.push(Node::Element(convert_element(jsx)));
        }
    }

    Ok(TsxDocument { root_nodes, imports })
}

/// The JSX element an arrow function returns, if it is a JSX component: a concise
/// body `() => (<JSX/>)` or an explicit `() => { return <JSX/>; }`. Parentheses
/// are transparent. `None` for a non-JSX arrow.
fn arrow_root_jsx<'a>(arrow: &'a ArrowFunctionExpression<'a>) -> Option<&'a JSXElement<'a>> {
    for st in &arrow.body.statements {
        match st {
            Statement::ExpressionStatement(es) => return expr_root_jsx(&es.expression),
            Statement::ReturnStatement(rs) => {
                return rs.argument.as_ref().and_then(expr_root_jsx);
            }
            _ => {}
        }
    }
    None
}

/// Unwrap parentheses to a root JSX element, if the expression is one.
fn expr_root_jsx<'a>(expr: &'a Expression<'a>) -> Option<&'a JSXElement<'a>> {
    match expr {
        Expression::JSXElement(j) => Some(j),
        Expression::ParenthesizedExpression(p) => expr_root_jsx(&p.expression),
        _ => None,
    }
}

/// Convert an oxc import declaration into the owned [`ImportDecl`] typed
/// reference (default / named / namespace bindings, in source order).
fn convert_import(decl: &oxc_ast::ast::ImportDeclaration) -> ImportDecl {
    let mut names = Vec::new();
    if let Some(specifiers) = &decl.specifiers {
        for spec in specifiers {
            match spec {
                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    let imported = match &s.imported {
                        ModuleExportName::IdentifierName(i) => i.name.to_string(),
                        ModuleExportName::IdentifierReference(i) => i.name.to_string(),
                        ModuleExportName::StringLiteral(s) => s.value.to_string(),
                    };
                    names.push(ImportName {
                        local: s.local.name.to_string(),
                        imported,
                        kind: ImportKind::Named,
                    });
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    names.push(ImportName {
                        local: s.local.name.to_string(),
                        imported: "default".to_string(),
                        kind: ImportKind::Default,
                    });
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    let local = s.local.name.to_string();
                    names.push(ImportName { imported: local.clone(), local, kind: ImportKind::Namespace });
                }
            }
        }
    }
    ImportDecl { source: decl.source.value.to_string(), names }
}

/// Parse a **multi-file app** into one combined [`TsxDocument`]: the app-level
/// file (`app_src`) supplies the root element (its tag + attributes — e.g.
/// `<App>` and any app-level props), and each entry of `screens` is a per-screen
/// source file whose own root element is spliced in as a child of that root, in
/// the given order.
///
/// This is the boundary for Highbay's multi-file app model (EDITOR_PLAN §7): the
/// hidden app-level file holds the screen registry/structure while each screen is
/// its own document, and the combined graph is derived from both. Keeping the
/// splice here (rather than in the consumer) keeps every `oxc_*` type quarantined
/// — callers get the same owned [`TsxDocument`] as [`parse_tsx`].
///
/// The returned document has exactly one root node: the app root element with the
/// screen root elements as its children (the app file's own children are
/// replaced by the screen elements — the screen files are the content authority).
/// A screen source with no root element, or an app source with no root element,
/// is an error.
pub fn parse_app(app_src: &str, screens: &[&str]) -> Result<TsxDocument, Vec<String>> {
    let app_doc = parse_tsx(app_src)?;
    let mut imports = app_doc.imports;
    let app_el = app_doc
        .root_nodes
        .into_iter()
        .find_map(|n| match n {
            Node::Element(e) => Some(e),
            _ => None,
        })
        .ok_or_else(|| vec!["app source has no root element".to_string()])?;

    let mut children = Vec::with_capacity(screens.len());
    for (i, src) in screens.iter().enumerate() {
        let mut doc = parse_tsx(src)?;
        imports.append(&mut doc.imports);
        let el = doc
            .root_nodes
            .into_iter()
            .find_map(|n| match n {
                Node::Element(e) => Some(e),
                _ => None,
            })
            .ok_or_else(|| vec![format!("screen source {i} has no root element")])?;
        children.push(Node::Element(el));
    }

    let combined = Element {
        tag: app_el.tag,
        type_args: app_el.type_args,
        attrs: app_el.attrs,
        children,
    };
    Ok(TsxDocument { root_nodes: vec![Node::Element(combined)], imports })
}

/// Extract every top-level TypeScript `interface` into an owned
/// [`InterfaceDecl`] (the Props-shape / DagNode contract).
///
/// Field types map onto [`TypeShape`]; `name?: T` optionality is preserved.
/// Unsupported members (index/method/call signatures) are skipped.
pub fn extract_interfaces(source: &str) -> Result<Vec<InterfaceDecl>, Vec<String>> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::tsx()).parse();

    if !ret.diagnostics.is_empty() {
        return Err(ret
            .diagnostics
            .into_iter()
            // `{e}`, not `{e:?}`. These strings are USER-FACING -- the IDE's
            // live-parse status strip renders them verbatim -- and the Debug
            // form spells the whole struct, so a typo appeared in the editor as
            // `Parse error: OxcDiagnostic { inner: OxcDiagnosticInner {
            // message: "Unexpected token", l...`, truncated mid-field. Display
            // is the rendered diagnostic oxc means a human to read.
            .map(|e| e.to_string())
            .collect());
    }

    let mut interfaces = Vec::new();
    for stmt in &ret.program.body {
        // `interface Foo {}` and `export interface Foo {}` both surface here.
        let decl = match stmt {
            Statement::TSInterfaceDeclaration(d) => Some(&**d),
            Statement::ExportNamedDeclaration(e) => match &e.declaration {
                Some(oxc_ast::ast::Declaration::TSInterfaceDeclaration(d)) => Some(&**d),
                _ => None,
            },
            _ => None,
        };
        if let Some(decl) = decl {
            interfaces.push(convert_interface(decl));
        }
    }

    Ok(interfaces)
}

fn convert_interface(decl: &oxc_ast::ast::TSInterfaceDeclaration) -> InterfaceDecl {
    InterfaceDecl {
        name: decl.id.name.to_string(),
        fields: signatures_to_fields(&decl.body.body),
    }
}

fn signatures_to_fields(sigs: &[TSSignature]) -> Vec<FieldDecl> {
    let mut fields = Vec::new();
    for sig in sigs {
        if let TSSignature::TSPropertySignature(prop) = sig {
            let name = match &prop.key {
                PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                PropertyKey::StringLiteral(s) => s.value.to_string(),
                _ => continue,
            };
            let ty = prop
                .type_annotation
                .as_ref()
                .map(|ann| type_shape(&ann.type_annotation))
                .unwrap_or(TypeShape::String);
            fields.push(FieldDecl {
                name,
                ty,
                optional: prop.optional,
            });
        }
    }
    fields
}

/// Map a `TSType` onto the owned [`TypeShape`] vocabulary.
fn type_shape(ty: &TSType) -> TypeShape {
    match ty {
        TSType::TSBooleanKeyword(_) => TypeShape::Bool,
        // TS `number` lowers to F64 by default (see dag::TypeShape docs).
        TSType::TSNumberKeyword(_) => TypeShape::F64,
        TSType::TSBigIntKeyword(_) => TypeShape::S64,
        TSType::TSStringKeyword(_) => TypeShape::String,
        TSType::TSArrayType(arr) => TypeShape::List(Box::new(type_shape(&arr.element_type))),
        TSType::TSParenthesizedType(p) => type_shape(&p.type_annotation),
        TSType::TSTypeLiteral(lit) => TypeShape::Record(signatures_to_fields(&lit.members)),
        TSType::TSUnionType(u) => union_shape(u),
        TSType::TSTypeReference(r) => reference_shape(r),
        // Anything else we don't model becomes an opaque named reference.
        _ => TypeShape::Named("unknown".to_string()),
    }
}

/// `T | undefined` / `T | null` → `Option<T>`; other unions collapse to the
/// first non-nullish member (best-effort — the semantic AST is deliberately
/// minimal).
fn union_shape(u: &oxc_ast::ast::TSUnionType) -> TypeShape {
    let mut nullish = false;
    let mut inner: Option<&TSType> = None;
    for t in &u.types {
        match t {
            TSType::TSUndefinedKeyword(_) | TSType::TSNullKeyword(_) => nullish = true,
            other => {
                if inner.is_none() {
                    inner = Some(other);
                }
            }
        }
    }
    match (inner, nullish) {
        (Some(t), true) => TypeShape::Option(Box::new(type_shape(t))),
        (Some(t), false) => type_shape(t),
        (None, _) => TypeShape::Named("unknown".to_string()),
    }
}

/// `Array<T>` → `List<T>`; anything else named → `Named`.
fn reference_shape(r: &oxc_ast::ast::TSTypeReference) -> TypeShape {
    let name = match &r.type_name {
        oxc_ast::ast::TSTypeName::IdentifierReference(id) => id.name.to_string(),
        _ => return TypeShape::Named("unknown".to_string()),
    };
    if name == "Array" {
        if let Some(args) = &r.type_arguments {
            if let Some(first) = args.params.first() {
                return TypeShape::List(Box::new(type_shape(first)));
            }
        }
    }
    TypeShape::Named(name)
}

fn convert_element(jsx: &JSXElement) -> Element {
    let tag = element_name(&jsx.opening_element.name);

    // `<List<Message> …>` — the opening tag's type arguments, through the same
    // `TypeShape` lowering an interface field's annotation takes.
    let type_args: Vec<TypeShape> = jsx
        .opening_element
        .type_arguments
        .as_ref()
        .map(|args| args.params.iter().map(type_shape).collect())
        .unwrap_or_default();

    let mut attrs = Vec::new();
    for attr in &jsx.opening_element.attributes {
        if let JSXAttributeItem::Attribute(a) = attr {
            let key = match &a.name {
                JSXAttributeName::Identifier(i) => i.name.to_string(),
                JSXAttributeName::NamespacedName(n) => {
                    format!("{}:{}", n.namespace.name, n.name.name)
                }
            };
            let value = match &a.value {
                None => AttrValue::Bool(true),
                Some(JSXAttributeValue::StringLiteral(s)) => AttrValue::Str(s.value.to_string()),
                Some(JSXAttributeValue::ExpressionContainer(c)) => c
                    .expression
                    .as_expression()
                    .map(attr_from_expr)
                    .unwrap_or(AttrValue::Opaque),
                _ => AttrValue::Opaque,
            };
            attrs.push((key, value));
        }
    }

    let mut children = Vec::new();
    for child in &jsx.children {
        push_child(&mut children, child);
    }

    Element {
        tag,
        type_args,
        attrs,
        children,
    }
}

fn element_name(name: &JSXElementName) -> String {
    match name {
        JSXElementName::Identifier(ident) => ident.name.to_string(),
        JSXElementName::IdentifierReference(ident) => ident.name.to_string(),
        JSXElementName::NamespacedName(n) => format!("{}:{}", n.namespace.name, n.name.name),
        JSXElementName::MemberExpression(mem) => {
            let obj = jsx_member_object(&mem.object);
            format!("{}.{}", obj, mem.property.name)
        }
        JSXElementName::ThisExpression(_) => "this".to_string(),
    }
}

fn jsx_member_object(obj: &oxc_ast::ast::JSXMemberExpressionObject) -> String {
    match obj {
        oxc_ast::ast::JSXMemberExpressionObject::IdentifierReference(i) => i.name.to_string(),
        oxc_ast::ast::JSXMemberExpressionObject::MemberExpression(m) => {
            format!("{}.{}", jsx_member_object(&m.object), m.property.name)
        }
        oxc_ast::ast::JSXMemberExpressionObject::ThisExpression(_) => "this".to_string(),
    }
}

fn push_child(out: &mut Vec<Node>, child: &JSXChild) {
    match child {
        JSXChild::Element(e) => out.push(Node::Element(convert_element(e))),
        JSXChild::Text(t) => {
            let txt = t.value.trim();
            if !txt.is_empty() {
                out.push(Node::Text(txt.to_string()));
            }
        }
        JSXChild::ExpressionContainer(c) => {
            if let Some(expr) = c.expression.as_expression() {
                match expr {
                    Expression::StringLiteral(s) => out.push(Node::Text(s.value.to_string())),
                    Expression::TemplateLiteral(t) => {
                        // Only lower plain (no-substitution) template strings.
                        if t.expressions.is_empty() && t.quasis.len() == 1 {
                            if let Some(raw) = t.quasis[0].value.cooked.as_ref() {
                                out.push(Node::Text(raw.to_string()));
                            }
                        }
                    }
                    other => {
                        if let Some(path) = expr_path(other) {
                            out.push(Node::Expr(path));
                        }
                    }
                }
            }
        }
        JSXChild::Fragment(frag) => {
            for c in &frag.children {
                push_child(out, c);
            }
        }
        JSXChild::Spread(_) => {}
    }
}

fn attr_from_expr(expr: &Expression) -> AttrValue {
    match expr {
        Expression::StringLiteral(s) => AttrValue::Str(s.value.to_string()),
        Expression::NumericLiteral(n) => AttrValue::Num(n.value),
        Expression::BooleanLiteral(b) => AttrValue::Bool(b.value),
        other => expr_path(other)
            .map(AttrValue::Binding)
            .unwrap_or(AttrValue::Opaque),
    }
}

/// Recover a dotted binding path from an identifier / static-member chain.
fn expr_path(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::StaticMemberExpression(m) => {
            Some(format!("{}.{}", expr_path(&m.object)?, m.property.name))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordered_attrs_and_children() {
        let doc =
            parse_tsx(r#"<List value={props.items} count={3} loading><Item>Hi</Item></List>"#)
                .expect("parse");
        assert_eq!(doc.root_nodes.len(), 1);
        let Node::Element(list) = &doc.root_nodes[0] else {
            panic!("expected element")
        };
        assert_eq!(list.tag, "List");
        // Source order preserved.
        assert_eq!(list.attrs[0].0, "value");
        assert_eq!(list.attrs[0].1, AttrValue::Binding("props.items".into()));
        assert_eq!(list.attrs[1].1, AttrValue::Num(3.0));
        assert_eq!(list.attrs[2].1, AttrValue::Bool(true));
        let Node::Element(item) = &list.children[0] else {
            panic!("expected item")
        };
        assert_eq!(item.tag, "Item");
        assert_eq!(item.children[0], Node::Text("Hi".into()));
    }

    /// A **generic JSX element** keeps its type argument. `<List<Message>>` is
    /// the author declaring what the list's rows are; before this the type
    /// parameter was present in the source and dropped on the floor.
    #[test]
    fn a_generic_element_carries_its_type_arguments() {
        let doc = parse_tsx(r#"<List<Message> value={chatFeed} window={24}><Item /></List>"#)
            .expect("a generic JSX element parses");
        let Node::Element(list) = &doc.root_nodes[0] else { panic!("expected an element") };
        assert_eq!(list.tag, "List", "the type argument is not part of the tag");
        assert_eq!(list.type_args, vec![TypeShape::Named("Message".into())]);
        // Attributes and children are untouched by the generic spelling.
        assert_eq!(list.attr("value"), Some(&AttrValue::Binding("chatFeed".into())));
        assert_eq!(list.children.len(), 1);

        // Self-closing, several arguments, and the built-in type vocabulary all
        // lower through the same mapping an interface field's type does.
        let doc = parse_tsx(r#"<Grid<Message, string> />"#).expect("parses");
        let Node::Element(grid) = &doc.root_nodes[0] else { panic!() };
        assert_eq!(
            grid.type_args,
            vec![TypeShape::Named("Message".into()), TypeShape::String],
        );
    }

    /// The ordinary spelling stays empty — nothing is invented for an element
    /// that wrote no type argument.
    #[test]
    fn a_plain_element_has_no_type_arguments() {
        let doc = parse_tsx(r#"<List value={chatFeed}><Item /></List>"#).expect("parses");
        let Node::Element(list) = &doc.root_nodes[0] else { panic!() };
        assert!(list.type_args.is_empty());
        let Node::Element(item) = &list.children[0] else { panic!() };
        assert!(item.type_args.is_empty());
    }

    #[test]
    fn captures_expression_and_string_children() {
        let doc = parse_tsx(r#"<Content>{"Hello {{name}}"}{user.email}</Content>"#).expect("parse");
        let Node::Element(c) = &doc.root_nodes[0] else {
            panic!()
        };
        assert_eq!(c.children[0], Node::Text("Hello {{name}}".into()));
        assert_eq!(c.children[1], Node::Expr("user.email".into()));
    }

    #[test]
    fn parse_is_deterministic() {
        let src = r#"<Row a="1" b={2} c={x.y} d />"#;
        let a = parse_tsx(src).unwrap();
        let b = parse_tsx(src).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn extracts_interfaces_with_types() {
        let src = r#"
            interface CounterProps {
                label: string;
                count: number;
                tags: string[];
                nickname?: string;
                bio: string | undefined;
                style: { bold: boolean };
                theme: Theme;
            }
        "#;
        let ifaces = extract_interfaces(src).expect("parse");
        assert_eq!(ifaces.len(), 1);
        let p = &ifaces[0];
        assert_eq!(p.name, "CounterProps");
        assert_eq!(p.fields[0].ty, TypeShape::String);
        assert_eq!(p.fields[1].ty, TypeShape::F64);
        assert_eq!(p.fields[2].ty, TypeShape::List(Box::new(TypeShape::String)));
        assert!(p.fields[3].optional);
        assert_eq!(
            p.fields[4].ty,
            TypeShape::Option(Box::new(TypeShape::String))
        );
        assert!(matches!(&p.fields[5].ty, TypeShape::Record(_)));
        assert_eq!(p.fields[6].ty, TypeShape::Named("Theme".into()));
    }

    #[test]
    fn extracts_exported_interface() {
        let ifaces = extract_interfaces("export interface P { ok: boolean; }").expect("parse");
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].fields[0].ty, TypeShape::Bool);
    }

    #[test]
    fn full_module_screen_parses_to_its_root_jsx_and_captures_imports() {
        // The worked example's shape: an import for a provider + a default-
        // exported arrow component that returns the screen's <Screen> JSX and
        // passes the imported provider as a `value=` prop on its <List>.
        let src = r#"
            import { libraryFeed } from "Library Feed";

            const Library = () => (
                <Screen name="Library" icon="library_books">
                    <List value={libraryFeed} window={20}>
                        <Item><Content>{"{{title}}"}</Content></Item>
                    </List>
                </Screen>
            );

            export default Library;
        "#;
        let doc = parse_tsx(src).expect("full module parses");
        // Exactly one root node — the <Screen> the default export returns.
        assert_eq!(doc.root_nodes.len(), 1);
        let Node::Element(screen) = &doc.root_nodes[0] else { panic!("root is the Screen element") };
        assert_eq!(screen.tag, "Screen");
        assert_eq!(screen.attr("name"), Some(&AttrValue::Str("Library".into())));
        // The <List> passes the imported provider as its bound value.
        let Node::Element(list) = &screen.children[0] else { panic!("first child is the List") };
        assert_eq!(list.tag, "List");
        assert_eq!(list.attr("value"), Some(&AttrValue::Binding("libraryFeed".into())));

        // The import is captured as a typed reference (module + local/imported).
        assert_eq!(doc.imports.len(), 1);
        assert_eq!(doc.imports[0].source, "Library Feed");
        assert_eq!(doc.imports[0].names.len(), 1);
        assert_eq!(doc.imports[0].names[0].local, "libraryFeed");
        assert_eq!(doc.imports[0].names[0].imported, "libraryFeed");
        assert_eq!(doc.imports[0].names[0].kind, crate::dag::ImportKind::Named);
    }

    #[test]
    fn export_default_arrow_and_aliased_default_imports_parse() {
        // Inline `export default () => (<JSX/>)` (no named const) + an aliased
        // named import and a default import.
        let src = r#"
            import Feed, { rows as feedRows } from "Feed Source";
            export default () => (<Screen name="Feed"><List value={feedRows}/></Screen>);
        "#;
        let doc = parse_tsx(src).expect("inline default-export arrow parses");
        assert_eq!(doc.root_nodes.len(), 1);
        let Node::Element(screen) = &doc.root_nodes[0] else { panic!() };
        assert_eq!(screen.attr("name"), Some(&AttrValue::Str("Feed".into())));
        // Default + aliased-named imports both captured.
        assert_eq!(doc.imports[0].names[0].kind, crate::dag::ImportKind::Default);
        assert_eq!(doc.imports[0].names[0].local, "Feed");
        assert_eq!(doc.imports[0].names[1].local, "feedRows");
        assert_eq!(doc.imports[0].names[1].imported, "rows");
    }

    #[test]
    fn bare_jsx_screen_still_parses_with_no_imports() {
        // Regression: the original bare-<Screen> shape is unchanged and carries
        // no imports (so every existing screen/widget keeps parsing).
        let doc = parse_tsx(r#"<Screen name="Home"><Item><Content>{"Hi"}</Content></Item></Screen>"#)
            .expect("bare screen parses");
        assert_eq!(doc.root_nodes.len(), 1);
        assert!(doc.imports.is_empty());
        let Node::Element(s) = &doc.root_nodes[0] else { panic!() };
        assert_eq!(s.tag, "Screen");
    }

    #[test]
    fn parse_app_splices_screen_files_under_the_app_root() {
        // The app-level file supplies the root <App> (with its attrs); each screen
        // file is spliced in as a child in order.
        let app = r#"<App><Screen name="Home" file="home.tsx" /></App>"#;
        let home = r#"<Screen name="Home" icon="home"><Item><Content>{"Hi"}</Content></Item></Screen>"#;
        let search = r#"<Screen name="Search"><Item><Content>{"Go"}</Content></Item></Screen>"#;
        let doc = parse_app(app, &[home, search]).expect("combined parse");

        assert_eq!(doc.root_nodes.len(), 1);
        let Node::Element(app_el) = &doc.root_nodes[0] else { panic!("root is the App element") };
        assert_eq!(app_el.tag, "App");
        // The app file's own <Screen> registry children are replaced by the two
        // screen documents (screen files are the content authority).
        assert_eq!(app_el.children.len(), 2, "one child per screen file, in order");
        let Node::Element(s0) = &app_el.children[0] else { panic!() };
        let Node::Element(s1) = &app_el.children[1] else { panic!() };
        assert_eq!(s0.tag, "Screen");
        assert_eq!(s0.attr("name"), Some(&AttrValue::Str("Home".into())));
        assert_eq!(s0.attr("icon"), Some(&AttrValue::Str("home".into())));
        // The screen content survived the splice.
        assert!(matches!(&s0.children[0], Node::Element(item) if item.tag == "Item"));
        assert_eq!(s1.attr("name"), Some(&AttrValue::Str("Search".into())));
    }

    #[test]
    fn parse_app_preserves_app_root_attributes() {
        // App-level props (a future depth/theme attribute) ride on the root.
        let app = r#"<App depth={2} />"#;
        let doc = parse_app(app, &[r#"<Screen name="Only" />"#]).expect("parse");
        let Node::Element(app_el) = &doc.root_nodes[0] else { panic!() };
        assert_eq!(app_el.attr("depth"), Some(&AttrValue::Num(2.0)));
        assert_eq!(app_el.children.len(), 1);
    }

    /// **What the parser produced is what serializes.** A document parsed from
    /// a screen-shaped source survives a serde round trip unchanged, which is
    /// the property downstream postcard encoding rests on: attributes in
    /// order, children in order, type arguments intact, imports intact.
    ///
    /// This is checked against *parsed* input rather than a hand-built tree
    /// because a hand-built tree can only contain what its author remembered
    /// to put in it; a parse of real source shape carries whatever the parser
    /// actually emits, including anything added later.
    #[test]
    fn a_parsed_document_round_trips_through_serde() {
        let src = r#"
            import { chatFeed } from "Chat Feed";

            <Screen name="Chat" icon="chat" section="Chats">
                <Column>
                    <List<Message> value={chatFeed} window={24} live>
                        <Item>
                            <Content>{"{{sender}}"}</Content>
                            {user.email}
                        </Item>
                    </List>
                    <MessageInput placeholder="Message #baychat-general" />
                </Column>
            </Screen>
        "#;
        let doc = parse_tsx(src).expect("screen parses");
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: TsxDocument = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(doc, back, "a parsed document did not survive serde");

        // Not vacuous: the document really does carry the tree, the generic
        // type argument and the import edge that make the assertion mean
        // something.
        let Node::Element(screen) = &back.root_nodes[0] else { panic!("root is an element") };
        let Node::Element(column) = &screen.children[0] else { panic!() };
        let Node::Element(list) = &column.children[0] else { panic!() };
        assert_eq!(list.type_args, vec![TypeShape::Named("Message".into())]);
        assert_eq!(
            list.attrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["value", "window", "live"],
            "attributes keep source order",
        );
        assert_eq!(back.imports[0].source, "Chat Feed");
    }

    #[test]
    fn parse_app_is_deterministic_and_reports_bad_screens() {
        let app = "<App />";
        let a = parse_app(app, &[r#"<Screen name="A" />"#]).unwrap();
        let b = parse_app(app, &[r#"<Screen name="A" />"#]).unwrap();
        assert_eq!(a, b);
        // A screen file with no root element is a reported error, not a panic.
        assert!(parse_app(app, &["   // just a comment"]).is_err());
    }
}
