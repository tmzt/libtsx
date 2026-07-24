//! The oxc-backed TSX parser (feature `parse`).
//!
//! Lowers TSX source into a fully-owned, oxc-free tree ([`TsxDocument`]) plus
//! extracts TypeScript `interface` declarations into the owned
//! [`crate::dag`] code-graph types ([`extract_interfaces`]).
//!
//! Design constraints (PLAN.md §4, Phase 7):
//! * **No `oxc_*` type appears in this module's public API** — callers get
//!   plain owned Rust data.
//! * **Deterministic output** — JSX attributes keep source order (a `Vec`,
//!   not a `HashMap`) so downstream node-graph serialization is stable.
//! * Expression children (`{binding}`) and string-literal children are
//!   captured (the old proof-of-concept dropped them).

use crate::dag::{FieldDecl, InterfaceDecl, TypeShape};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Expression, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXElementName, PropertyKey, Statement, TSSignature, TSType,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// A parsed JSX element with ordered attributes and children.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// The tag name (e.g. `List`, `Content`, `A.B` for member tags).
    pub tag: String,
    /// Attributes in source order. Ordered (not a map) for deterministic
    /// downstream serialization.
    pub attrs: Vec<(String, AttrValue)>,
    /// Child nodes in source order.
    pub children: Vec<Node>,
}

impl Element {
    /// Look up an attribute value by name (first match).
    pub fn attr(&self, name: &str) -> Option<&AttrValue> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

/// A JSX attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    /// `attr="text"` or `attr={"text"}`.
    Str(String),
    /// `attr={42}`.
    Num(f64),
    /// `attr={true}` or a bare `attr` (valueless → `true`).
    Bool(bool),
    /// `attr={ident}` / `attr={props.items}` — a data-binding path.
    Binding(String),
    /// An expression we don't lower (element/fragment/complex expr).
    Opaque,
}

/// A node in a parsed JSX tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// A nested element.
    Element(Element),
    /// Literal text (JSX text, or a `{"string literal"}` child). Carries the
    /// verbatim text, including any `{{ }}` Markdown-templating placeholders
    /// which the highbay_ui `<Content>` layer interprets.
    Text(String),
    /// A `{binding}` expression child — a data-binding path.
    Expr(String),
}

/// A parsed, fully-owned TSX document (no oxc arena references).
#[derive(Debug, Clone, PartialEq)]
pub struct TsxDocument {
    pub root_nodes: Vec<Node>,
}

/// Parse TSX source into an owned [`TsxDocument`].
///
/// Only top-level JSX expression statements contribute root nodes; interface
/// and other declarations are ignored here (see [`extract_interfaces`]).
pub fn parse_tsx(source: &str) -> Result<TsxDocument, Vec<String>> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::tsx()).parse();

    if !ret.diagnostics.is_empty() {
        return Err(ret
            .diagnostics
            .into_iter()
            .map(|e| format!("{e:?}"))
            .collect());
    }

    let mut root_nodes = Vec::new();
    for stmt in &ret.program.body {
        if let Statement::ExpressionStatement(expr_stmt) = stmt {
            match &expr_stmt.expression {
                Expression::JSXElement(jsx) => {
                    root_nodes.push(Node::Element(convert_element(jsx)));
                }
                Expression::JSXFragment(frag) => {
                    for child in &frag.children {
                        push_child(&mut root_nodes, child);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(TsxDocument { root_nodes })
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
            .map(|e| format!("{e:?}"))
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
}
