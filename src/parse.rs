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
//! * **Effect bindings are produced here, not inferred later.** An
//!   [`is_event_binding`] attribute's value is parsed as one call resolving to
//!   a granted host import and emitted as [`AttrValue::NamedEffect`]; there is
//!   no pass that later decides an [`AttrValue::Opaque`] was really an effect
//!   (LIBHBUI_PLAN Rules 46a, 48).
//! * **Configuration is a context, not a second entry point.** What a load
//!   offers a source is [`ParseCtx`], built through [`ParseCtx::builder`] and
//!   passed to whichever parse entry point the caller needs (Rule 49).

use crate::dag::{
    AttrValue, EffectError, Element, FieldDecl, FuncSig, HostEffects, ImportDecl, ImportKind,
    ImportName, InterfaceDecl, NamedEffect, Node, TsxDocument, TypeShape, is_event_binding,
    is_host_namespace,
};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, ExportDefaultDeclarationKind, Expression, ImportDeclarationSpecifier,
    JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement, JSXElementName,
    ModuleExportName, PropertyKey, Statement, TSSignature, TSType,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// Everything a parse can refuse.
///
/// Three kinds, kept apart because they are different facts: oxc could not read
/// the source, it read it and the source declared something that cannot mean
/// what it says, or a file that had to supply a root element did not. The free
/// [`parse_tsx`] / [`parse_app`] flatten all of them into the `Vec<String>`
/// their callers have always taken; [`ParseCtx::parse_tsx`] and
/// [`ParseCtx::parse_app`] hand them back **typed**, which is what lets a
/// refusal be asserted on rather than string-matched.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// oxc's diagnostics, rendered for a human (see [`parse_tsx`]).
    Syntax(Vec<String>),
    /// An effect binding or a host import that cannot mean what it says
    /// (LIBHBUI_PLAN Rules 46a, 48).
    Effect(EffectError),
    /// A source [`ParseCtx::parse_app`] required a root JSX element from and
    /// did not get one.
    NoRootElement {
        /// Which screen, or `None` for the app-level file.
        screen: Option<usize>,
    },
}

impl ParseError {
    /// The messages [`parse_tsx`] reports - one per diagnostic, or one for the
    /// refusal.
    pub fn messages(self) -> Vec<String> {
        match self {
            Self::Syntax(messages) => messages,
            other => vec![other.to_string()],
        }
    }
}

impl From<EffectError> for ParseError {
    fn from(e: EffectError) -> Self {
        Self::Effect(e)
    }
}

impl std::fmt::Display for ParseError {
    /// ASCII only - these strings reach the editor's live-parse status strip.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(messages) => write!(f, "{}", messages.join("; ")),
            Self::Effect(e) => write!(f, "{e}"),
            Self::NoRootElement { screen: None } => write!(f, "app source has no root element"),
            Self::NoRootElement { screen: Some(i) } => {
                write!(f, "screen source {i} has no root element")
            }
        }
    }
}

impl std::error::Error for ParseError {}

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
    ParseCtx::default()
        .parse_tsx(source)
        .map_err(ParseError::messages)
}

/// **How a parse is configured** - what this load offers the source
/// (LIBHBUI_PLAN Rule 49).
///
/// One value, built once and passed to whichever entry point a caller needs, so
/// a capability enabled here is available *wherever parsing happens*. The shape
/// it replaces was a function per capability - `parse_tsx_with(src, &host)`
/// beside `parse_tsx(src)` - and the cost of that shape had already been paid:
/// [`ParseCtx::parse_app`]'s predecessor granted nothing, not by decision but
/// because it was a third function nobody extended, so the multi-file path
/// could not express an effect at all.
///
/// **The builder is the only way to configure one.** The fields are private and
/// there is no setter, so a context is either the default (offering nothing) or
/// one a [`ParseCtxBuilder`] produced - which is what stops a capability being
/// enabled by a route some other entry point forgets:
///
/// ```compile_fail,E0451
/// use libtsx::{HostEffects, ParseCtx};
///
/// let ctx = ParseCtx { effects: Some(HostEffects::none()) };
/// ```
///
/// The default offers **nothing**, which is the honest one: a source calling an
/// effect it was never given has named a capability it does not have.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseCtx {
    /// The effect surface, or `None` for a load that does not offer one at all.
    effects: Option<HostEffects>,
}

/// Builds a [`ParseCtx`]. See [`ParseCtx::builder`].
#[derive(Debug, Clone, Default)]
pub struct ParseCtxBuilder {
    effects: Option<HostEffects>,
}

impl ParseCtxBuilder {
    /// Offer the **effect surface**, granting nothing through it yet.
    ///
    /// The distinction this draws is between an embedding that has no
    /// capabilities to give and one that withheld a particular capability - a
    /// source importing `host:x` gets [`EffectError::EffectsNotOffered`] in the
    /// first case and [`EffectError::UnknownHostNamespace`] in the second.
    /// Neither is a parse that quietly succeeds.
    pub fn enable_effects(mut self) -> Self {
        self.effects.get_or_insert_with(HostEffects::none);
        self
    }

    /// Grant these host effects, **offering the surface** in the same step.
    ///
    /// A grant is the stronger statement, so it implies [`Self::enable_effects`]
    /// rather than needing it: a caller that sets a host and forgets to enable
    /// would otherwise have configured a capability the parse ignores, which is
    /// the failure mode a single context exists to remove.
    pub fn set_host(mut self, host: HostEffects) -> Self {
        self.effects = Some(host);
        self
    }

    /// The configured context.
    pub fn build(self) -> ParseCtx {
        ParseCtx {
            effects: self.effects,
        }
    }
}

impl ParseCtx {
    /// Start configuring a context.
    pub fn builder() -> ParseCtxBuilder {
        ParseCtxBuilder::default()
    }

    /// What a source may call, or `None` if this load offers no effects.
    fn host(&self) -> Option<&HostEffects> {
        self.effects.as_ref()
    }

    /// [`parse_tsx`] **in this context**, with the refusal handed back typed.
    ///
    /// An `on..` attribute ([`is_event_binding`]) is parsed as one call
    /// resolving through the module's import chain to a signature this context
    /// grants, and becomes [`AttrValue::NamedEffect`]. Nothing about that is
    /// deferred: an attribute that announced an effect and cannot carry one is
    /// refused **here**, with a typed [`EffectError`], rather than surviving as
    /// an [`AttrValue::Opaque`] that silently does nothing (Rule 46a).
    pub fn parse_tsx(&self, source: &str) -> Result<TsxDocument, ParseError> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, source, SourceType::tsx()).parse();

        if !ret.diagnostics.is_empty() {
            return Err(ParseError::Syntax(
                ret.diagnostics
                    .into_iter()
                    // `{e}`, not `{e:?}`. These strings are USER-FACING -- the IDE's
                    // live-parse status strip renders them verbatim -- and the Debug
                    // form spells the whole struct, so a typo appeared in the editor as
                    // `Parse error: OxcDiagnostic { inner: OxcDiagnosticInner {
                    // message: "Unexpected token", l...`, truncated mid-field. Display
                    // is the rendered diagnostic oxc means a human to read.
                    .map(|e| e.to_string())
                    .collect(),
            ));
        }

        // Pass 0: the import edges, and the effect scope they open. This runs
        // BEFORE any element is converted, because an effect name resolves against
        // the whole module's imports rather than the ones written above the element
        // that calls it - and because a scope built as the elements go by would
        // depend on statement order for its answers.
        let mut imports = Vec::new();
        for stmt in &ret.program.body {
            if let Statement::ImportDeclaration(decl) = stmt {
                imports.push(convert_import(decl));
            }
        }
        let scope = EffectScope::build(&imports, self)?;

        let mut root_nodes = Vec::new();
        // Pass 1: bare JSX statements and a table of
        // `const Name = () => (<JSX/>)` arrow components (for export-default-by-name).
        let mut arrow_components: Vec<(&str, &JSXElement)> = Vec::new();
        for stmt in &ret.program.body {
            match stmt {
                Statement::ExpressionStatement(expr_stmt) => match &expr_stmt.expression {
                    Expression::JSXElement(jsx) => {
                        root_nodes.push(Node::Element(convert_element(jsx, &scope)?))
                    }
                    Expression::JSXFragment(frag) => {
                        for child in &frag.children {
                            push_child(&mut root_nodes, child, &scope)?;
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
                    root_nodes.push(Node::Element(convert_element(jsx, &scope)?));
                }
            }
        }

        // Lenient fallback: a module with a single `const` arrow component and no
        // export/bare-JSX still yields its JSX (so a mid-edit missing `export default`
        // doesn't blank the preview).
        if root_nodes.is_empty() {
            if let Some((_, jsx)) = arrow_components.first() {
                root_nodes.push(Node::Element(convert_element(jsx, &scope)?));
            }
        }

        Ok(TsxDocument { root_nodes, imports })
    }

    /// Parse a **multi-file app** into one combined [`TsxDocument`]: the
    /// app-level file (`app_src`) supplies the root element (its tag +
    /// attributes - e.g. `<App>` and any app-level props), and each entry of
    /// `screens` is a per-screen source file whose own root element is spliced
    /// in as a child of that root, in the given order.
    ///
    /// This is the boundary for Highbay's multi-file app model (EDITOR_PLAN
    /// §7): the hidden app-level file holds the screen registry/structure while
    /// each screen is its own document, and the combined graph is derived from
    /// both. Keeping the splice here (rather than in the consumer) keeps every
    /// `oxc_*` type quarantined - callers get the same owned [`TsxDocument`] as
    /// [`ParseCtx::parse_tsx`].
    ///
    /// The returned document has exactly one root node: the app root element
    /// with the screen root elements as its children (the app file's own
    /// children are replaced by the screen elements - the screen files are the
    /// content authority). A screen source with no root element, or an app
    /// source with no root element, is a [`ParseError::NoRootElement`].
    ///
    /// **The context is what it grants**, and that is the whole reason it
    /// exists: every file here parses in *this* context, so an effect the
    /// embedding offered is available in a screen file. Its predecessor was a
    /// third free function that granted nothing - not by decision, but because
    /// nobody extended it - and the multi-file path could not express an effect
    /// at all (Rule 49).
    pub fn parse_app(&self, app_src: &str, screens: &[&str]) -> Result<TsxDocument, ParseError> {
        let app_doc = self.parse_tsx(app_src)?;
        let mut imports = app_doc.imports;
        let app_el = root_element(app_doc.root_nodes)
            .ok_or(ParseError::NoRootElement { screen: None })?;

        let mut children = Vec::with_capacity(screens.len());
        for (i, src) in screens.iter().enumerate() {
            let mut doc = self.parse_tsx(src)?;
            imports.append(&mut doc.imports);
            let el = root_element(doc.root_nodes).ok_or(ParseError::NoRootElement {
                screen: Some(i),
            })?;
            children.push(Node::Element(el));
        }

        let combined = Element {
            tag: app_el.tag,
            type_args: app_el.type_args,
            attrs: app_el.attrs,
            children,
        };
        Ok(TsxDocument {
            root_nodes: vec![Node::Element(combined)],
            imports,
        })
    }
}

/// The first root node that is an element.
fn root_element(root_nodes: Vec<Node>) -> Option<Element> {
    root_nodes.into_iter().find_map(|n| match n {
        Node::Element(e) => Some(e),
        _ => None,
    })
}

// --- the effect scope (LIBHBUI_PLAN Rules 46a, 48) ----------------------------

/// The effects a module may call: its `host:` **import declarations**, checked
/// against what the load granted.
///
/// Built once per parse, from the module's own [`ImportDecl`] edges - *where
/// each name came from*. A local name is carried alongside the namespace it
/// came from and the name that namespace exports, so `import { navigate as go }`
/// resolves `go` to `navigate`'s signature and the alias is gone by the time
/// anything downstream reads it.
///
/// **The declaration and the signature are two things, chained here.** This
/// struct holds the first ([`ImportDecl`]-derived bindings) and borrows the
/// second ([`HostEffects`]'s [`FuncSig`]s); [`EffectScope::resolve`] is the
/// chain. Nothing may shortcut from a callee straight to a signature - that
/// would be a name typed by something it was never imported from.
struct EffectScope<'a> {
    /// The grant, or `None` if the context offers no effect surface at all -
    /// in which case `locals` is empty, because every `host:` import was
    /// refused before it could add one.
    host: Option<&'a HostEffects>,
    /// `(local, namespace, imported)`, in source order.
    locals: Vec<(String, String, String)>,
}

impl<'a> EffectScope<'a> {
    /// The scope a module's imports open, refusing the ways a host import can
    /// fail to be one (Rules 48, 49).
    ///
    /// Non-`host:` imports are left entirely alone: whether a specifier names a
    /// real project Script is the consumer's question, not the parser's.
    fn build(imports: &[ImportDecl], ctx: &'a ParseCtx) -> Result<Self, EffectError> {
        let mut locals = Vec::new();
        for decl in imports {
            if !is_host_namespace(&decl.source) {
                continue;
            }
            // A load that offers no effects at all is a different fact from one
            // that grants some other namespace, and says so: the source asked
            // for a surface this embedding does not have.
            let Some(host) = ctx.host() else {
                return Err(EffectError::EffectsNotOffered {
                    source: decl.source.clone(),
                });
            };
            // A `host:` specifier that nothing granted is neither a Script to
            // compile nor a capability to grant, so it is refused at load
            // rather than producing bindings that can never fire.
            if host.namespace(&decl.source).is_none() {
                return Err(EffectError::UnknownHostNamespace {
                    source: decl.source.clone(),
                });
            }
            for name in &decl.names {
                // `import * as fx` / `import fx from`: `fx.navigate(...)` is a
                // member expression and a callee is a flat name, so the only
                // way such a binding could reach one is as the string
                // "fx.navigate" - structure smuggled into a name.
                if name.kind != ImportKind::Named {
                    return Err(EffectError::NotANamedImport {
                        source: decl.source.clone(),
                        local: name.local.clone(),
                        kind: name.kind,
                    });
                }
                if host.declares(&decl.source, &name.imported).is_none() {
                    return Err(EffectError::UndeclaredHostImport {
                        source: decl.source.clone(),
                        imported: name.imported.clone(),
                    });
                }
                locals.push((
                    name.local.clone(),
                    decl.source.clone(),
                    name.imported.clone(),
                ));
            }
        }
        Ok(Self {
            host: ctx.host(),
            locals,
        })
    }

    /// The signature a local name resolves to, through the local->imported
    /// chain.
    fn resolve(&self, local: &str) -> Option<&FuncSig> {
        let (_, namespace, imported) = self.locals.iter().find(|(name, _, _)| name == local)?;
        self.host?.declares(namespace, imported)
    }
}

/// Lower an `on..` attribute's value into [`AttrValue::NamedEffect`], or refuse
/// it (Rules 46a, 48).
///
/// The attribute already announced itself as an event binding, so every exit
/// from here is either an effect or an error - there is deliberately no path
/// that yields [`AttrValue::Opaque`].
fn effect_attr(
    attr: &str,
    value: Option<&JSXAttributeValue>,
    scope: &EffectScope,
) -> Result<AttrValue, EffectError> {
    let not_a_call = || EffectError::NotACall {
        attr: attr.to_string(),
    };
    // `onTap="Chat"`, `onTap`, `onTap={goChat}`, `onTap={() => ..}` and
    // `onTap={<X/>}` all land here: the value is one CALL or it is a mistake.
    let Some(JSXAttributeValue::ExpressionContainer(container)) = value else {
        return Err(not_a_call());
    };
    let Some(expr) = container.expression.as_expression() else {
        return Err(not_a_call());
    };
    let Expression::CallExpression(call) = unparen(expr) else {
        return Err(not_a_call());
    };

    let callee = unparen(&call.callee);
    let Expression::Identifier(local) = callee else {
        // Anything that is not a bare name cannot resolve: a member expression
        // (`fx.navigate`) is refused here rather than flattened into a callee
        // string, and a computed callee has no name to resolve at all.
        return Err(EffectError::Unresolved {
            attr: attr.to_string(),
            callee: expr_path(callee).unwrap_or_else(|| "a computed callee".to_string()),
        });
    };
    let Some(sig) = scope.resolve(local.name.as_str()) else {
        return Err(EffectError::Unresolved {
            attr: attr.to_string(),
            callee: local.name.to_string(),
        });
    };

    if call.arguments.len() != sig.params.len() {
        return Err(EffectError::ArgCount {
            attr: attr.to_string(),
            effect: sig.name.clone(),
            declared: sig.params.len(),
            given: call.arguments.len(),
        });
    }
    let mut args = Vec::with_capacity(sig.params.len());
    for (index, (arg, param)) in call.arguments.iter().zip(&sig.params).enumerate() {
        let lowered = arg
            .as_expression()
            .ok_or(ArgFail::NotALiteral)
            .and_then(|expr| lower_arg(unparen(expr), &param.ty))
            .map_err(|fail| match fail {
                ArgFail::NotALiteral => EffectError::ArgNotALiteral {
                    attr: attr.to_string(),
                    effect: sig.name.clone(),
                    index,
                },
                ArgFail::WrongType => EffectError::ArgType {
                    attr: attr.to_string(),
                    effect: sig.name.clone(),
                    index,
                    declared: param.ty.clone(),
                },
            })?;
        args.push(lowered);
    }

    Ok(AttrValue::NamedEffect(NamedEffect {
        // The RESOLVED name: an alias is spent here and never travels.
        name: sig.name.clone(),
        args,
    }))
}

/// Why an argument could not be lowered.
enum ArgFail {
    /// Not a literal at all - an identifier, a member expression, a call. An
    /// effect call is not an expression language (Rule 46a); a computation
    /// belongs in a Module.
    NotALiteral,
    /// A literal the declared parameter type cannot hold.
    WrongType,
}

/// Lower one literal argument **against its declared type**, so the signature
/// decides what a number becomes rather than the parser guessing (Rule 48).
///
/// Every parameter is supplied: optionality of a host import's parameter is not
/// modelled, and an omitted argument is an [`EffectError::ArgCount`].
fn lower_arg(expr: &Expression, declared: &TypeShape) -> Result<crate::dag::Expr, ArgFail> {
    use crate::dag::Expr as E;
    match expr {
        Expression::StringLiteral(s) => match declared {
            TypeShape::String => Ok(E::LitStr(s.value.to_string())),
            _ => Err(ArgFail::WrongType),
        },
        Expression::BooleanLiteral(b) => match declared {
            TypeShape::Bool => Ok(E::LitBool(b.value)),
            _ => Err(ArgFail::WrongType),
        },
        Expression::NumericLiteral(n) => match declared {
            TypeShape::S32 => whole(n.value, i32::MIN as f64, i32::MAX as f64)
                .map(|v| E::LitS32(v as i32))
                .ok_or(ArgFail::WrongType),
            // 2^53: past it an f64 literal is no longer the integer it was
            // written as, and a silently rounded destination or id is the
            // defect Rule 40 refuses for the same reason.
            TypeShape::S64 => whole(n.value, -9_007_199_254_740_992.0, 9_007_199_254_740_992.0)
                .map(|v| E::LitS64(v as i64))
                .ok_or(ArgFail::WrongType),
            TypeShape::F32 => Ok(E::LitF32(n.value as f32)),
            TypeShape::F64 => Ok(E::LitF64(n.value)),
            _ => Err(ArgFail::WrongType),
        },
        _ => Err(ArgFail::NotALiteral),
    }
}

/// `value` as a whole number inside `[lo, hi]`, or `None`.
fn whole(value: f64, lo: f64, hi: f64) -> Option<f64> {
    (value.fract() == 0.0 && (lo..=hi).contains(&value)).then_some(value)
}

/// Unwrap parentheses. `onTap={(navigate("Chat"))}` is the same call.
fn unparen<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
    match expr {
        Expression::ParenthesizedExpression(p) => unparen(&p.expression),
        other => other,
    }
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

/// [`ParseCtx::parse_app`] in the default context - a multi-file app parsed
/// with **nothing granted**.
///
/// The convenience form, exactly as [`parse_tsx`] is: it exists so no caller
/// that never wanted a capability has to name a context. A caller that does
/// want one builds it ([`ParseCtx::builder`]) and calls the method, which is
/// the same context [`ParseCtx::parse_tsx`] takes.
pub fn parse_app(app_src: &str, screens: &[&str]) -> Result<TsxDocument, Vec<String>> {
    ParseCtx::default()
        .parse_app(app_src, screens)
        .map_err(ParseError::messages)
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

fn convert_element(jsx: &JSXElement, scope: &EffectScope) -> Result<Element, EffectError> {
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
        // A SPREAD is refused, not skipped. `{...handlers}` where
        // `handlers = { onTap: navigate("Chat") }` reaches an element as no
        // attribute at all: the loop below never sees an `on..` NAME, so
        // `is_event_binding` is never consulted and every effect refusal is
        // blind to it - the erasure Rule 46a closed, by the one route it does
        // not watch. It could not be honoured even if it resolved: a spread
        // attribute set cannot be checked against declared props.
        let JSXAttributeItem::Attribute(a) = attr else {
            return Err(EffectError::SpreadAttribute { tag });
        };
        let key = match &a.name {
            JSXAttributeName::Identifier(i) => i.name.to_string(),
            JSXAttributeName::NamespacedName(n) => {
                format!("{}:{}", n.namespace.name, n.name.name)
            }
        };
        // **Detection and carriage are one step** (Rule 46a): the name
        // announces an event binding, so the value is lowered as an effect
        // right here. There is no later pass that reinterprets an
        // `AttrValue::Opaque`, which is exactly why an `on..` attribute can
        // never quietly become one.
        let value = if is_event_binding(&key) {
            effect_attr(&key, a.value.as_ref(), scope)?
        } else {
            match &a.value {
                None => AttrValue::Bool(true),
                Some(JSXAttributeValue::StringLiteral(s)) => AttrValue::Str(s.value.to_string()),
                Some(JSXAttributeValue::ExpressionContainer(c)) => c
                    .expression
                    .as_expression()
                    .map(attr_from_expr)
                    .unwrap_or(AttrValue::Opaque),
                _ => AttrValue::Opaque,
            }
        };
        attrs.push((key, value));
    }

    let mut children = Vec::new();
    for child in &jsx.children {
        push_child(&mut children, child, scope)?;
    }

    Ok(Element {
        tag,
        type_args,
        attrs,
        children,
    })
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

fn push_child(
    out: &mut Vec<Node>,
    child: &JSXChild,
    scope: &EffectScope,
) -> Result<(), EffectError> {
    match child {
        JSXChild::Element(e) => out.push(Node::Element(convert_element(e, scope)?)),
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
                push_child(out, c, scope)?;
            }
        }
        JSXChild::Spread(_) => {}
    }
    Ok(())
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

    // --- effect bindings (LIBHBUI_PLAN Rules 46a, 48) -------------------------
    //
    // Every source below goes through the real parser. Several of them are
    // hand-written malformations, which is Rule 43's exception - **the point IS
    // the shape**: what is being ruled out is source an authoring surface could
    // produce, so there is nothing else to parse them from.

    /// What the host grants these tests: one string-taking `navigate`, and a
    /// second effect with a numeric and a boolean parameter, so the type check
    /// is exercised on more than one shape.
    fn granted() -> HostEffects {
        let mut host = HostEffects::granting(
            "host:effects",
            vec![FuncSig {
                name: "navigate".into(),
                params: vec![FieldDecl {
                    name: "to".into(),
                    ty: TypeShape::String,
                    optional: false,
                }],
                result: None,
            }],
        );
        host.grant(
            "host:sheets",
            vec![FuncSig {
                name: "dismiss".into(),
                params: vec![
                    FieldDecl {
                        name: "after".into(),
                        ty: TypeShape::S32,
                        optional: false,
                    },
                    FieldDecl {
                        name: "animated".into(),
                        ty: TypeShape::Bool,
                        optional: false,
                    },
                ],
                result: None,
            }],
        );
        host
    }

    /// The context those grants are offered through - one value, built the one
    /// way there is (Rule 49).
    fn ctx() -> ParseCtx {
        ParseCtx::builder().set_host(granted()).build()
    }

    /// The one attribute of the tree, for a source with exactly one element.
    fn only_attr(src: &str) -> AttrValue {
        let doc = ctx().parse_tsx(src).expect("parses");
        let Node::Element(el) = &doc.root_nodes[0] else {
            panic!("root is an element")
        };
        el.attrs
            .iter()
            .find(|(k, _)| is_event_binding(k))
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

    /// **The whole authoring surface** (Rule 46a): `onTap={navigate("Chat")}`.
    ///
    /// The value the attribute carries is the *resolved* host name and its
    /// lowered arguments - not the local name, not the source text, and not an
    /// `Opaque`.
    #[test]
    fn an_event_binding_carries_a_named_effect() {
        assert_eq!(
            only_attr(
                r#"
                import { navigate } from "host:effects";
                <Action id="a" onTap={navigate("Chat")} />
                "#
            ),
            AttrValue::NamedEffect(NamedEffect {
                name: "navigate".into(),
                args: vec![crate::dag::Expr::LitStr("Chat".into())],
            })
        );

        // An ALIAS is spent at the parse: `go` never travels. This is what
        // "named" buys - the name in the graph is the host import's, so a
        // reader resolves nothing and two sources that alias differently
        // produce one value.
        assert_eq!(
            only_attr(
                r#"
                import { navigate as go } from "host:effects";
                <Action id="a" onTap={go("Chat")} />
                "#
            ),
            AttrValue::NamedEffect(NamedEffect {
                name: "navigate".into(),
                args: vec![crate::dag::Expr::LitStr("Chat".into())],
            })
        );

        // The recognition rule is the attribute's NAME, not the tag and not a
        // list: a second effect, on a different attribute, from a second
        // namespace, needs nothing added anywhere. Its arguments lower against
        // the DECLARED types - `0` becomes `LitS32`, not a float.
        assert_eq!(
            only_attr(
                r#"
                import { dismiss } from "host:sheets";
                <Sheet id="a" onLongPress={dismiss(250, true)} />
                "#
            ),
            AttrValue::NamedEffect(NamedEffect {
                name: "dismiss".into(),
                args: vec![
                    crate::dag::Expr::LitS32(250),
                    crate::dag::Expr::LitBool(true),
                ],
            })
        );

        // And an ordinary attribute is untouched by any of it.
        let doc = ctx().parse_tsx(
            r#"
            import { navigate } from "host:effects";
            <Action id="a" height={56} onTap={navigate("Chat")} label="Chat" />
            "#,
        )
        .expect("parses");
        let Node::Element(el) = &doc.root_nodes[0] else { panic!() };
        assert_eq!(
            el.attrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["id", "height", "onTap", "label"],
            "attributes keep source order",
        );
        assert_eq!(el.attr("height"), Some(&AttrValue::Num(56.0)));
        assert_eq!(el.attr("label"), Some(&AttrValue::Str("Chat".into())));
    }

    /// **Refusal 1.** An `on..` attribute whose value is not a call.
    ///
    /// Each of these parsed to `AttrValue::Opaque` or `AttrValue::Binding`
    /// before the producer existed - an effect erased, and erased *silently*,
    /// which is the failure this refusal is for. The withdrawn
    /// `onTap={namedHandler}` spelling is in the list on purpose: it is the
    /// most plausible wrong thing to write.
    #[test]
    fn an_event_binding_that_is_not_a_call_is_refused() {
        for value in [
            r#"onTap={goChat}"#,                 // a bare identifier
            r#"onTap={() => navigate("Chat")}"#, // an arrow function
            r#"onTap="Chat""#,                   // a string
            r#"onTap"#,                          // valueless (would be `true`)
            r#"onTap={"Chat"}"#,                 // a string in a container
            r#"onTap={props.destination}"#,      // a data binding
            r#"onTap={<Action/>}"#,              // an element
            r#"onTap={navigate("Chat") && x}"#,  // a call inside an expression
        ] {
            let src = format!(
                r#"
                import {{ navigate }} from "host:effects";
                <Action id="a" {value} />
                "#
            );
            assert_eq!(
                refusal(&src),
                EffectError::NotACall {
                    attr: "onTap".into()
                },
                "`{value}` was not refused as a non-call",
            );
        }
    }

    /// **Refusal 2.** A callee that does not resolve, through the import chain,
    /// to a declared host import.
    #[test]
    fn a_callee_that_resolves_to_no_host_import_is_refused() {
        // Never imported at all.
        assert_eq!(
            refusal(r#"<Action id="a" onTap={navigate("Chat")} />"#),
            EffectError::Unresolved {
                attr: "onTap".into(),
                callee: "navigate".into(),
            }
        );
        // Imported from a Script rather than granted by the host: a Script
        // import is COMPILED and a host import is GRANTED, and only the second
        // can be an effect (Rule 48).
        assert_eq!(
            refusal(
                r#"
                import { navigate } from "Nav Helpers";
                <Action id="a" onTap={navigate("Chat")} />
                "#
            ),
            EffectError::Unresolved {
                attr: "onTap".into(),
                callee: "navigate".into(),
            }
        );
        // The LOCAL name is what resolves: an alias means the exported name no
        // longer names anything in this module.
        assert_eq!(
            refusal(
                r#"
                import { navigate as go } from "host:effects";
                <Action id="a" onTap={navigate("Chat")} />
                "#
            ),
            EffectError::Unresolved {
                attr: "onTap".into(),
                callee: "navigate".into(),
            }
        );
    }

    /// **Refusal 3.** Argument count and type, against the `FuncSig`.
    ///
    /// This is the check that would have caught `navigate(target: S32)`
    /// disagreeing with libhbui's string destinations by machine instead of by
    /// reading.
    #[test]
    fn arguments_are_checked_against_the_declared_signature() {
        let with = |call: &str| {
            format!(
                r#"
                import {{ navigate }} from "host:effects";
                import {{ dismiss }} from "host:sheets";
                <Action id="a" onTap={{{call}}} />
                "#
            )
        };
        assert_eq!(
            refusal(&with("navigate()")),
            EffectError::ArgCount {
                attr: "onTap".into(),
                effect: "navigate".into(),
                declared: 1,
                given: 0,
            }
        );
        assert_eq!(
            refusal(&with(r#"navigate("Chat", "Threads")"#)),
            EffectError::ArgCount {
                attr: "onTap".into(),
                effect: "navigate".into(),
                declared: 1,
                given: 2,
            }
        );
        // A number where a stored symbol is declared - the exact disagreement.
        assert_eq!(
            refusal(&with("navigate(3)")),
            EffectError::ArgType {
                attr: "onTap".into(),
                effect: "navigate".into(),
                index: 0,
                declared: TypeShape::String,
            }
        );
        // And the reverse, on the second parameter, so the index is not
        // always zero.
        assert_eq!(
            refusal(&with(r#"dismiss(250, "yes")"#)),
            EffectError::ArgType {
                attr: "onTap".into(),
                effect: "dismiss".into(),
                index: 1,
                declared: TypeShape::Bool,
            }
        );
        // A fractional literal is not an S32, and is refused rather than
        // truncated: a silently rounded argument is a different call.
        assert_eq!(
            refusal(&with("dismiss(2.5, true)")),
            EffectError::ArgType {
                attr: "onTap".into(),
                effect: "dismiss".into(),
                index: 0,
                declared: TypeShape::S32,
            }
        );
        // Not a literal at all. An effect call is not an expression language;
        // a computation is a Module, referenced opaquely (Rule 46a).
        for arg in ["props.destination", "1 + 2", "f()", "`Chat`"] {
            assert_eq!(
                refusal(&with(&format!("navigate({arg})"))),
                EffectError::ArgNotALiteral {
                    attr: "onTap".into(),
                    effect: "navigate".into(),
                    index: 0,
                },
                "`{arg}` was not refused as a non-literal",
            );
        }
    }

    /// **Refusal 4.** A host namespace bound by `import * as` (or a default
    /// import): `fx.navigate(...)` cannot reach a flat callee except as the
    /// string `"fx.navigate"`, which is structure smuggled into a name.
    #[test]
    fn a_host_namespace_bound_as_a_namespace_is_refused() {
        assert_eq!(
            refusal(
                r#"
                import * as fx from "host:effects";
                <Action id="a" onTap={fx.navigate("Chat")} />
                "#
            ),
            EffectError::NotANamedImport {
                source: "host:effects".into(),
                local: "fx".into(),
                kind: ImportKind::Namespace,
            }
        );
        assert_eq!(
            refusal(
                r#"
                import fx from "host:effects";
                <Action id="a" onTap={fx("Chat")} />
                "#
            ),
            EffectError::NotANamedImport {
                source: "host:effects".into(),
                local: "fx".into(),
                kind: ImportKind::Default,
            }
        );
        // The refusal is about the IMPORT, so it fires whether or not anything
        // calls through it - a binding that could never name an effect is a
        // mistake at the line that wrote it.
        assert_eq!(
            refusal(
                r#"
                import * as fx from "host:effects";
                <Action id="a" />
                "#
            ),
            EffectError::NotANamedImport {
                source: "host:effects".into(),
                local: "fx".into(),
                kind: ImportKind::Namespace,
            }
        );
        // A member-expression callee with no host import behind it is
        // unresolved rather than accepted - the flat-callee rule holds even
        // where no namespace import is in sight.
        assert_eq!(
            refusal(r#"<Action id="a" onTap={fx.navigate("Chat")} />"#),
            EffectError::Unresolved {
                attr: "onTap".into(),
                callee: "fx.navigate".into(),
            }
        );
    }

    /// **Rule 48 at the import line.** A `host:` specifier names a granted
    /// namespace and a name that namespace declares, or it is refused at load -
    /// rather than producing a binding that can never fire.
    #[test]
    fn a_host_import_is_granted_or_refused() {
        assert_eq!(
            refusal(
                r#"
                import { navigate } from "host:telemetry";
                <Action id="a" />
                "#
            ),
            EffectError::UnknownHostNamespace {
                source: "host:telemetry".into(),
            }
        );
        assert_eq!(
            refusal(
                r#"
                import { teleport } from "host:effects";
                <Action id="a" />
                "#
            ),
            EffectError::UndeclaredHostImport {
                source: "host:effects".into(),
                imported: "teleport".into(),
            }
        );

        // A Script import is not touched by any of this: it has a source, it is
        // compiled, and resolving it is the consumer's job as it always was.
        let doc = ctx().parse_tsx(
            r#"
            import { libraryFeed } from "Library Feed";
            <List value={libraryFeed} />
            "#,
        )
        .expect("a Script import is not a host import");
        assert_eq!(doc.imports[0].source, "Library Feed");
    }

    /// **Nothing granted is the honest default.** [`parse_tsx`] grants nothing,
    /// so a source that calls an effect has named a capability it was not
    /// given - and says so, rather than erasing the call.
    ///
    /// The two "nothings" are different facts and say so separately (Rule 49):
    /// a context with the surface *enabled* and this namespace ungranted is
    /// [`EffectError::UnknownHostNamespace`]; the default context, which offers
    /// no surface at all, is [`EffectError::EffectsNotOffered`].
    #[test]
    fn with_nothing_granted_an_effect_does_not_resolve() {
        let src = r#"
            import { navigate } from "host:effects";
            <Action id="a" onTap={navigate("Chat")} />
        "#;
        assert_eq!(
            ParseCtx::builder().enable_effects().build().parse_tsx(src),
            Err(ParseError::Effect(EffectError::UnknownHostNamespace {
                source: "host:effects".into(),
            }))
        );
        assert_eq!(
            ParseCtx::default().parse_tsx(src),
            Err(ParseError::Effect(EffectError::EffectsNotOffered {
                source: "host:effects".into(),
            }))
        );
        // Through the untyped entry point the same refusal arrives as a
        // message, so no caller silently gets a document.
        let messages = parse_tsx(src).expect_err("refused");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("host:effects"), "{messages:?}");
        assert!(messages[0].is_ascii(), "{messages:?}");

        // And a source with no effects at all is unaffected by the rule.
        assert!(parse_tsx(r#"<Screen name="Home"><Item /></Screen>"#).is_ok());
    }

    /// **What the parser produced is what serializes**, effects included. The
    /// same claim `a_parsed_document_round_trips_through_serde` makes, on the
    /// one attribute value that has a nested shape.
    #[test]
    fn a_parsed_effect_survives_serde() {
        let doc = ctx().parse_tsx(
            r#"
            import { navigate } from "host:effects";
            <Drawer id="d1"><Action id="d2" onTap={navigate("Chat")} /></Drawer>
            "#,
        )
        .expect("parses");
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: TsxDocument = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(doc, back);

        let Node::Element(drawer) = &back.root_nodes[0] else { panic!() };
        let Node::Element(row) = &drawer.children[0] else { panic!() };
        assert_eq!(
            row.attr("onTap"),
            Some(&AttrValue::NamedEffect(NamedEffect {
                name: "navigate".into(),
                args: vec![crate::dag::Expr::LitStr("Chat".into())],
            })),
            "the effect did not survive the round trip",
        );
    }

    /// Every refusal renders ASCII (Rule 39): these strings reach the editor's
    /// live-parse status strip and panic dumps.
    #[test]
    fn every_effect_refusal_renders_ascii() {
        for e in [
            EffectError::NotACall { attr: "onTap".into() },
            EffectError::Unresolved {
                attr: "onTap".into(),
                callee: "navigate".into(),
            },
            EffectError::ArgCount {
                attr: "onTap".into(),
                effect: "navigate".into(),
                declared: 1,
                given: 0,
            },
            EffectError::ArgType {
                attr: "onTap".into(),
                effect: "navigate".into(),
                index: 0,
                declared: TypeShape::String,
            },
            EffectError::ArgNotALiteral {
                attr: "onTap".into(),
                effect: "navigate".into(),
                index: 0,
            },
            EffectError::NotANamedImport {
                source: "host:effects".into(),
                local: "fx".into(),
                kind: ImportKind::Namespace,
            },
            EffectError::UnknownHostNamespace {
                source: "host:x".into(),
            },
            EffectError::UndeclaredHostImport {
                source: "host:effects".into(),
                imported: "teleport".into(),
            },
            EffectError::EffectsNotOffered {
                source: "host:effects".into(),
            },
            EffectError::SpreadAttribute { tag: "Action".into() },
        ] {
            assert!(e.to_string().is_ascii(), "{e:?}");
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn parse_app_is_deterministic_and_reports_bad_screens() {
        let app = "<App />";
        let a = parse_app(app, &[r#"<Screen name="A" />"#]).unwrap();
        let b = parse_app(app, &[r#"<Screen name="A" />"#]).unwrap();
        assert_eq!(a, b);
        // A screen file with no root element is a reported error, not a panic,
        // and it says WHICH file - the typed refusal carries the index.
        assert_eq!(
            ParseCtx::default().parse_app(app, &["   // just a comment"]),
            Err(ParseError::NoRootElement { screen: Some(0) })
        );
        assert_eq!(
            parse_app("// no app root", &[r#"<Screen name="A" />"#]).unwrap_err(),
            vec!["app source has no root element".to_string()],
        );
    }

    /// **RULE 49's whole argument.** One context, configured once, serves every
    /// entry point - so a capability enabled for [`ParseCtx::parse_tsx`] is
    /// available to [`ParseCtx::parse_app`] without anybody extending a third
    /// function.
    ///
    /// The multi-file path could not express an effect at all before this: its
    /// predecessor called the ungranted `parse_tsx`, so a screen file's
    /// `onTap={navigate("Chat")}` was refused however the embedding was
    /// configured. Both halves are asserted here, because "the app parses" on
    /// its own would also pass if the parse had simply become lenient.
    #[test]
    fn one_context_serves_parse_app_as_well_as_parse_tsx() {
        let app = "<App />";
        let screen = r#"
            import { navigate } from "host:effects";
            <Screen name="Home"><Action id="a" onTap={navigate("Chat")} /></Screen>
        "#;

        let doc = ctx().parse_app(app, &[screen]).expect("the grant reaches a screen file");
        let Node::Element(root) = &doc.root_nodes[0] else {
            panic!("the app root is an element")
        };
        let Node::Element(spliced) = &root.children[0] else {
            panic!("the screen is spliced in as an element")
        };
        let Node::Element(action) = &spliced.children[0] else {
            panic!("the action is the screen's child")
        };
        assert_eq!(
            action.attr("onTap"),
            Some(&AttrValue::NamedEffect(NamedEffect {
                name: "navigate".into(),
                args: vec![crate::dag::Expr::LitStr("Chat".into())],
            })),
            "the effect did not survive the splice",
        );

        // And the default context still refuses the same source, so what made
        // the difference was the grant rather than a lenient parse.
        assert_eq!(
            ParseCtx::default().parse_app(app, &[screen]),
            Err(ParseError::Effect(EffectError::EffectsNotOffered {
                source: "host:effects".into(),
            }))
        );
    }

    /// **A spread attribute is refused** (Rule 46a's remaining door).
    ///
    /// `const handlers = { onTap: navigate("Chat") }` then
    /// `<Action {...handlers}/>` used to parse clean and yield an `<Action>`
    /// with **no binding**: the attribute loop only ever saw
    /// `JSXAttributeItem::Attribute`, so a `SpreadAttribute` fell off the end
    /// and `is_event_binding` was never consulted. That is the erasure the
    /// `NamedEffect` producer exists to close, arriving by the one route none
    /// of its refusals watch.
    ///
    /// Hand-written source, and Rule 43's exception applies - **the point IS
    /// the shape**: what is ruled out is a spelling the authoring surface must
    /// refuse, so there is nothing else to parse it from.
    #[test]
    fn a_spread_attribute_is_refused() {
        // The erasure itself: an effect that reaches the element as nothing.
        assert_eq!(
            refusal(
                r#"
                import { navigate } from "host:effects";
                const handlers = { onTap: navigate("Chat") };
                <Action id="a" {...handlers} />
                "#
            ),
            EffectError::SpreadAttribute { tag: "Action".into() }
        );
        // The refusal is about the SPREAD, not about effects: an attribute set
        // spread from a value cannot be checked against declared props either,
        // so it is refused with nothing granted and on a nested element too.
        assert_eq!(
            ParseCtx::default().parse_tsx(r#"<Screen name="Home"><Item {...props} /></Screen>"#),
            Err(ParseError::Effect(EffectError::SpreadAttribute {
                tag: "Item".into()
            }))
        );
        // An ordinary attribute list is untouched by the rule.
        assert!(parse_tsx(r#"<Item id="a" label="Hi" />"#).is_ok());
    }
}
