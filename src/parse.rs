use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_ast::ast::{JSXChild, JSXElementName, JSXAttributeItem, JSXAttributeValue};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    pub props: HashMap<String, String>,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Element(Element),
    Text(String),
}

/// A parsed, fully-owned TSX document structure that doesn't depend on oxc's arena.
#[derive(Debug, Clone)]
pub struct TsxDocument {
    pub root_nodes: Vec<Node>,
}

pub fn parse_tsx(source: &str) -> Result<TsxDocument, Vec<String>> {
    let allocator = Allocator::default();
    let source_type = SourceType::tsx();
    
    let ret = Parser::new(&allocator, source, source_type).parse();
    
    if !ret.diagnostics.is_empty() {
        return Err(ret.diagnostics.into_iter().map(|e| format!("{:?}", e)).collect());
    }

    let mut root_nodes = Vec::new();
    
    for stmt in &ret.program.body {
        // We look for ExpressionStatement containing JSXElement or JSXFragment
        if let oxc_ast::ast::Statement::ExpressionStatement(expr_stmt) = stmt {
            match &expr_stmt.expression {
                oxc_ast::ast::Expression::JSXElement(jsx) => {
                    root_nodes.push(Node::Element(convert_element(jsx)));
                }
                oxc_ast::ast::Expression::JSXFragment(frag) => {
                    for child in &frag.children {
                        if let oxc_ast::ast::JSXChild::Element(elem) = child {
                            root_nodes.push(Node::Element(convert_element(elem)));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(TsxDocument { root_nodes })
}

fn convert_element(jsx: &oxc_ast::ast::JSXElement) -> Element {
    let tag = match &jsx.opening_element.name {
        JSXElementName::Identifier(ident) => ident.name.to_string(),
        JSXElementName::IdentifierReference(ident) => ident.name.to_string(),
        JSXElementName::MemberExpression(mem) => {
            // Simplified member expression (e.g. A.B)
            let obj = match &mem.object {
                oxc_ast::ast::JSXMemberExpressionObject::IdentifierReference(i) => i.name.to_string(),
                _ => "Unknown".to_string(),
            };
            format!("{}.{}", obj, mem.property.name)
        }
        _ => "Unknown".to_string(),
    };

    let mut props = HashMap::new();
    for attr in &jsx.opening_element.attributes {
        if let JSXAttributeItem::Attribute(a) = attr {
            let key = match &a.name {
                oxc_ast::ast::JSXAttributeName::Identifier(i) => i.name.to_string(),
                _ => "unknown".to_string(),
            };
            
            let val = if let Some(v) = &a.value {
                match v {
                    JSXAttributeValue::StringLiteral(s) => s.value.to_string(),
                    JSXAttributeValue::ExpressionContainer(expr) => {
                        if let Some(inner) = expr.expression.as_expression() {
                            if let oxc_ast::ast::Expression::NumericLiteral(num) = inner {
                                format!("{}", num.value)
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        }
                    }
                    _ => String::new(),
                }
            } else {
                "true".to_string() // boolean prop without value
            };
            props.insert(key, val);
        }
    }

    let mut children = Vec::new();
    for child in &jsx.children {
        match child {
            JSXChild::Element(e) => children.push(Node::Element(convert_element(e))),
            JSXChild::Text(t) => {
                let txt = t.value.trim();
                if !txt.is_empty() {
                    children.push(Node::Text(txt.to_string()));
                }
            }
            // Add support for JSXExpressionContainer if needed (e.g. {var})
            _ => {}
        }
    }

    Element {
        tag,
        props,
        children,
    }
}
