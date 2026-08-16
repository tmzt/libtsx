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
    AttrValue, BindingExpr, BindingLiteral, BindingParam, EffectError, BlockArrow, BlockStmt,
    Element, FieldDecl, FuncSig, ImportDecl, ImportKind, ImportName, InterfaceDecl, NamedEffect,
    Node, ParserHost, Resolution, TsxDocument, TypeShape, is_event_binding, is_host_namespace,
};
use std::sync::Arc;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, ExportDefaultDeclarationKind, Expression, ImportDeclarationSpecifier,
    BinaryOperator, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXElementName, LogicalOperator, ModuleExportName, PropertyKey, Statement, TSSignature, TSType,
};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};

/// Everything a parse can refuse.
///
/// Four kinds, kept apart because they are different facts: oxc could not read
/// the source, it read it and the owned expression vocabulary declines what it
/// says, it read it and the source declared something that cannot mean what it
/// says, or a file that had to supply a root element did not. The free
/// [`parse_tsx`] / [`parse_app`] flatten all of them into the `Vec<String>`
/// their callers have always taken; [`ParseCtx::parse_tsx`] and
/// [`ParseCtx::parse_app`] hand them back **typed**, which is what lets a
/// refusal be asserted on rather than string-matched.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// oxc's diagnostics, rendered for a human (see [`parse_tsx`]).
    Syntax(Vec<String>),
    /// The text is valid TypeScript and the owned vocabulary has no shape for
    /// it - a spread, a computed key, a template literal.
    ///
    /// Only [`BindingExpr`]'s `TryFrom<&str>` produces this: a refusal reached
    /// through a DOCUMENT is about an attribute, and carries the attribute's
    /// name as [`EffectError::BindingSyntax`]. A bare fragment has no attribute
    /// to name, and inventing one would be a lie about where the text came
    /// from.
    Binding(String),
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
            Self::Binding(message) => write!(f, "unsupported binding expression: {message}"),
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
/// **What it holds is a provider, not a grant** (Rule 52). The context asks its
/// [`ParserHost`] what a specifier resolves to and never enumerates the
/// answers, which is what keeps every name in an embedding's model out of this
/// crate.
///
/// **The builder is the only way to configure one.** Every field is private and
/// none has a setter, so a context is either the default (offering nothing) or
/// one a [`ParseCtxBuilder`] produced - which is what stops a capability being
/// enabled by a route some other entry point forgets:
///
/// ```compile_fail
/// use libtsx::ParseCtx;
///
/// let ctx = ParseCtx { host: None, retain_comments: true };
/// ```
///
/// (`compile_fail` without an error code: rustc reports a *private field* as
/// E0451 when one field is private and as an uncoded "cannot construct with
/// struct literal syntax" once there are two, so pinning the code would make
/// this doctest fail the next time a capability is added - which is the
/// opposite of what it is here to defend.)
///
/// The default offers **nothing**, which is the honest one: a source calling an
/// effect it was never given has named a capability it does not have.
#[derive(Clone, Default)]
pub struct ParseCtx {
    /// The embedding's provider, or `None` for a load that offers no host
    /// surface at all.
    host: Option<Arc<dyn ParserHost>>,
    /// Whether authored comments survive as [`Node::Comment`]. See
    /// [`ParseCtxBuilder::retain_comments`].
    retain_comments: bool,
}

impl std::fmt::Debug for ParseCtx {
    /// A provider is a behaviour and cannot print what it would answer, so this
    /// says whether there is one rather than pretending to describe it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseCtx")
            .field("host", &self.host.is_some())
            .field("retain_comments", &self.retain_comments)
            .finish()
    }
}

/// Builds a [`ParseCtx`]. See [`ParseCtx::builder`].
#[derive(Clone, Default)]
pub struct ParseCtxBuilder {
    host: Option<Arc<dyn ParserHost>>,
    retain_comments: bool,
}

/// The provider [`ParseCtxBuilder::enable_effects`] installs: the surface is
/// offered, and it resolves nothing.
struct NoGrants;

impl ParserHost for NoGrants {
    fn resolve(&self, _specifier: &str) -> Option<Resolution<'_>> {
        None
    }
}

impl ParseCtxBuilder {
    /// Offer the **host surface**, granting nothing through it yet.
    ///
    /// The distinction this draws is between an embedding that has no
    /// capabilities to give and one that withheld a particular capability - a
    /// source importing `host:x` gets [`EffectError::EffectsNotOffered`] in the
    /// first case and [`EffectError::UnknownHostNamespace`] in the second.
    /// Neither is a parse that quietly succeeds.
    pub fn enable_effects(mut self) -> Self {
        self.host.get_or_insert_with(|| Arc::new(NoGrants));
        self
    }

    /// Set the embedding's [`ParserHost`], **offering the surface** in the same
    /// step.
    ///
    /// A provider is the stronger statement, so it implies
    /// [`Self::enable_effects`] rather than needing it: a caller that sets a
    /// host and forgets to enable would otherwise have configured a capability
    /// the parse ignores, which is the failure mode a single context exists to
    /// remove.
    pub fn set_host(mut self, host: impl ParserHost + 'static) -> Self {
        self.host = Some(Arc::new(host));
        self
    }

    /// Keep the source's **comments**, as [`Node::Comment`] nodes in the tree
    /// they were written in.
    ///
    /// **Off by default, and that default is the load-bearing half.** Highbay
    /// has two parses of the same file with two different jobs:
    ///
    /// * The **publish** path lowers a source to a node-graph that ships. It
    ///   does not ask for comments, so no published `.hbdef` and no
    ///   `NodeGraph` can contain a [`Node::Comment`] at all - not by a filter
    ///   somewhere downstream that could be forgotten, but because the variant
    ///   is never constructed on that path.
    /// * The **editor** path shows the author their source, and that source is
    ///   emitted from the graph ([`crate::emit_tsx_document`]) rather than read
    ///   off disk. It asks, because a comment-free graph emits a gutted file:
    ///   `data/projects/default/screens/home.tsx` is 23 comment lines out of
    ///   52, and `data/projects/baychat/screens/chat.tsx` is 45 out of 82.
    ///
    /// This is a switch on the ONE context rather than a second entry point,
    /// for the reason [`ParseCtx`] exists at all (Rule 49): a capability
    /// spelled as its own function is a capability the next entry point does
    /// not have.
    ///
    /// # What is retained, and what is not
    ///
    /// Retained: every comment at the top level of the module (before or
    /// between the imports, the interfaces and the JSX), and every
    /// `{/* comment */}` written as a JSX child. Each becomes one
    /// [`Node::Comment`] holding the comment VERBATIM, delimiters included, in
    /// its authored position.
    ///
    /// Not retained: a comment written *inside* an expression - between an
    /// attribute and its value, or inside a list render arrow's parentheses.
    /// Those sit in places the element tree has no node for, and inventing one
    /// would mean the graph carrying a position it cannot re-emit.
    pub fn retain_comments(mut self) -> Self {
        self.retain_comments = true;
        self
    }

    /// The configured context.
    pub fn build(self) -> ParseCtx {
        ParseCtx {
            host: self.host,
            retain_comments: self.retain_comments,
        }
    }
}

impl ParseCtx {
    /// Start configuring a context.
    pub fn builder() -> ParseCtxBuilder {
        ParseCtxBuilder::default()
    }

    /// The embedding's provider, or `None` if this load offers no host surface.
    fn host(&self) -> Option<&dyn ParserHost> {
        self.host.as_deref()
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
        let low = Lowering {
            scope: EffectScope::build(&imports, self)?,
            comments: self
                .retain_comments
                .then(|| Comments::of(source, &ret.program.comments)),
        };

        // Every root node, paired with WHERE IN THE SOURCE it began, and every
        // span the element tree took ownership of. Both are for the comment
        // merge below and cost nothing when it does not run: a comment inside
        // one of the owned spans belongs to the tree (`push_child` already
        // placed it), and the position is what puts the loose ones back in
        // authored order without reordering anything else.
        let mut roots: Vec<(u32, Node)> = Vec::new();
        let mut owned: Vec<Span> = Vec::new();
        // Pass 1: bare JSX statements and a table of
        // `const Name = () => (<JSX/>)` arrow components (for export-default-by-name).
        let mut arrow_components: Vec<(&str, &JSXElement)> = Vec::new();
        for stmt in &ret.program.body {
            match stmt {
                Statement::ExpressionStatement(expr_stmt) => match &expr_stmt.expression {
                    Expression::JSXElement(jsx) => {
                        owned.push(jsx.span);
                        roots.push((jsx.span.start, Node::Element(convert_element(jsx, &low)?)));
                    }
                    Expression::JSXFragment(frag) => {
                        owned.push(frag.span);
                        let mut kids = Vec::new();
                        for child in &frag.children {
                            push_child(&mut kids, child, &low, None)?;
                        }
                        roots.extend(kids.into_iter().map(|n| (frag.span.start, n)));
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
                    owned.push(jsx.span);
                    roots.push((jsx.span.start, Node::Element(convert_element(jsx, &low)?)));
                }
            }
        }

        // Lenient fallback: a module with a single `const` arrow component and no
        // export/bare-JSX still yields its JSX (so a mid-edit missing `export default`
        // doesn't blank the preview).
        if roots.is_empty() {
            if let Some((_, jsx)) = arrow_components.first() {
                owned.push(jsx.span);
                roots.push((jsx.span.start, Node::Element(convert_element(jsx, &low)?)));
            }
        }

        let root_nodes = match &low.comments {
            None => roots.into_iter().map(|(_, node)| node).collect(),
            Some(comments) => comments.merge_roots(roots, &owned),
        };

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
    /// **Top-level comments do not survive the splice**, even under
    /// [`ParseCtxBuilder::retain_comments`]: "exactly one root node" is this
    /// method's contract, and a header comment has no element to hang off once
    /// several files have been combined into one. That is not a loss on any
    /// path that matters - this is the PUBLISH shape, whose parse does not
    /// retain comments in the first place, and the editor's source comes from
    /// one file's own [`ParseCtx::parse_tsx`]. Comments written as JSX children
    /// are inside a screen's root element and travel with it.
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
///
/// A [`Node::Comment`] root is skipped rather than refused: a source whose
/// header comment was retained still has a root element, and the wildcard here
/// says so deliberately.
fn root_element(root_nodes: Vec<Node>) -> Option<Element> {
    root_nodes.into_iter().find_map(|n| match n {
        Node::Element(e) => Some(e),
        _ => None,
    })
}

// --- what the JSX lowering carries -------------------------------------------

/// The two things every element conversion needs: what the module's imports
/// resolved to, and (when this parse retains them) where its comments are.
///
/// One value rather than two parameters, for the reason [`ParseCtx`] is one
/// value rather than a function per capability: a lowering that grew a third
/// thing to carry would otherwise grow a third parameter on every recursive
/// call, and the one call site that forgot it would be the one that silently
/// dropped what it carried.
struct Lowering<'a> {
    /// What each imported name was imported from (Rules 46a, 48).
    scope: EffectScope<'a>,
    /// The source's comments, or `None` when this parse does not retain them.
    comments: Option<Comments<'a>>,
}

/// Where the source's comments are, so a lowering can place them.
///
/// Holds SPANS and the source, never extracted strings: a comment's text is
/// `source[span]` verbatim, delimiters included, which is what makes emit a
/// copy rather than a reconstruction (see [`Node::Comment`]).
struct Comments<'a> {
    /// The source the spans index into.
    source: &'a str,
    /// Every comment's span, in source order (oxc collects them lexically).
    spans: Vec<Span>,
}

impl<'a> Comments<'a> {
    fn of(source: &'a str, comments: &[oxc_ast::Comment]) -> Self {
        Self {
            source,
            spans: comments.iter().map(|c| c.span).collect(),
        }
    }

    /// The comment at `span`, verbatim.
    fn text(&self, span: Span) -> &'a str {
        &self.source[span.start as usize..span.end as usize]
    }

    /// Every comment written inside `outer` - the shape a `{/* comment */}`
    /// child is read with, where `outer` is the expression container's own
    /// span.
    fn within(&self, outer: Span) -> impl Iterator<Item = &'a str> + '_ {
        self.spans
            .iter()
            .filter(move |s| outer.start <= s.start && s.end <= outer.end)
            .map(|s| self.text(*s))
    }

    /// The root node list: every root the passes produced, with the comments
    /// that are NOT inside the element tree put back where they were written.
    ///
    /// `roots` carries each node's source position and `owned` the spans the
    /// element tree took over. A comment inside an owned span was already
    /// placed as a child by [`push_child`] and must not be added twice.
    ///
    /// **Nothing here reorders the roots.** They are emitted in the order the
    /// passes produced them, and each loose comment goes before the first root
    /// that starts after it. Sorting by position would have been tidier and
    /// would have changed the node order of a module with both a bare JSX
    /// statement and an export-default component - a document shape difference
    /// caused by asking for comments, which is exactly what a retention switch
    /// must not do.
    fn merge_roots(&self, roots: Vec<(u32, Node)>, owned: &[Span]) -> Vec<Node> {
        let loose: Vec<Span> = self
            .spans
            .iter()
            .copied()
            .filter(|s| !owned.iter().any(|o| o.start <= s.start && s.end <= o.end))
            .collect();
        let mut out = Vec::with_capacity(roots.len() + loose.len());
        let mut next = loose.iter();
        let mut pending = next.next();
        for (start, node) in roots {
            while let Some(span) = pending.filter(|s| s.start < start) {
                out.push(Node::Comment(self.text(*span).to_string()));
                pending = next.next();
            }
            out.push(node);
        }
        while let Some(span) = pending {
            out.push(Node::Comment(self.text(*span).to_string()));
            pending = next.next();
        }
        out
    }
}

// --- the effect scope (LIBHBUI_PLAN Rules 46a, 48) ----------------------------

/// What a module's **import declarations** resolved to, asked of the
/// embedding's [`ParserHost`] once per parse.
///
/// Built from the module's own [`ImportDecl`] edges - *where each name came
/// from*. A local name bound from a granted host namespace is carried alongside
/// the signature that namespace declares for it, so `import { frobnicate as fb }`
/// resolves `fb` to `frobnicate`'s signature and the alias is gone by the time
/// anything downstream reads it.
///
/// **The declaration and the signature are two things, chained here.** The
/// declaration binds a local name; the signature types the call. Nothing may
/// shortcut from a callee straight to a signature - that would be a name typed
/// by something it was never imported from - so [`EffectScope::resolve`] walks
/// the declaration to a specifier first and only then to what the provider
/// declares under it.
///
/// **A name imported from a Script or a package is remembered too**
/// ([`EffectScope::foreign`]). Only a host namespace can supply an effect, but
/// "you never imported that" and "you imported that from something compiled"
/// are different facts about the source, and the provider is what lets the
/// parse tell them apart.
struct EffectScope<'a> {
    /// `(local, namespace, signature)` for every name bound from a granted host
    /// namespace, in source order. The namespace is kept because it is half the
    /// effect's identity (Rule 48) and the only half a callee cannot recover:
    /// two granted namespaces may each export a `frobnicate`.
    granted: Vec<(&'a str, &'a str, &'a FuncSig)>,
    /// `(local, specifier)` for every name bound from a Script or a package.
    foreign: Vec<(&'a str, &'a str)>,
}

/// What a local name was imported from.
enum Bound<'a> {
    /// A granted host import: the namespace it was granted under, and the
    /// signature that types the call. Both travel, because the qualified name
    /// is the identity.
    Host {
        /// The specifier the grant answered for.
        namespace: &'a str,
        /// What it declares this name to be.
        sig: &'a FuncSig,
    },
    /// A Script or a package - imported, and unable to supply an effect.
    Foreign(&'a str),
}

impl<'a> EffectScope<'a> {
    /// The scope a module's imports open, refusing the ways an import can fail
    /// to be one the parse can honour (Rules 48, 49, 52).
    ///
    /// A non-host specifier the provider does not resolve is left entirely
    /// alone: whether it names a real project Script has never been the
    /// parser's question, and refusing it here would break every load that has
    /// no Script registry to answer with.
    fn build(imports: &'a [ImportDecl], ctx: &'a ParseCtx) -> Result<Self, EffectError> {
        let mut granted = Vec::new();
        let mut foreign = Vec::new();
        for decl in imports {
            // A load that offers no host surface at all is a different fact
            // from one whose provider does not resolve this specifier, and says
            // so: the source asked for a surface this embedding does not have.
            let Some(host) = ctx.host() else {
                if is_host_namespace(&decl.source) {
                    return Err(EffectError::EffectsNotOffered {
                        source: decl.source.clone(),
                    });
                }
                continue;
            };
            let resolved = host.resolve(&decl.source);
            let sigs = match resolved {
                Some(Resolution::Host(sigs)) => {
                    // The scheme is what tells the three answers apart before
                    // anything consults the provider, so a grant spelled
                    // without one could never be reached by an import.
                    if !is_host_namespace(&decl.source) {
                        return Err(EffectError::GrantedWithoutScheme {
                            source: decl.source.clone(),
                        });
                    }
                    sigs
                }
                // Compiled or fetched elsewhere, and supplying the parse
                // nothing. A `host:` specifier answered this way is not granted
                // at all, which is the same refusal as a specifier the provider
                // does not know: a capability that resolves to something with a
                // source behind it is not a capability.
                other => {
                    if is_host_namespace(&decl.source) {
                        return Err(EffectError::UnknownHostNamespace {
                            source: decl.source.clone(),
                        });
                    }
                    if other.is_some() {
                        foreign.extend(
                            decl.names
                                .iter()
                                .map(|name| (name.local.as_str(), decl.source.as_str())),
                        );
                    }
                    continue;
                }
            };
            for name in &decl.names {
                // `import * as fx` / `import fx from`: `fx.frobnicate(...)` is
                // a member expression and a callee is a flat name, so the only
                // way such a binding could reach one is as the string
                // "fx.frobnicate" - structure smuggled into a name.
                if name.kind != ImportKind::Named {
                    return Err(EffectError::NotANamedImport {
                        source: decl.source.clone(),
                        local: name.local.clone(),
                        kind: name.kind,
                    });
                }
                let Some(sig) = sigs.iter().find(|sig| sig.name == name.imported) else {
                    return Err(EffectError::UndeclaredHostImport {
                        source: decl.source.clone(),
                        imported: name.imported.clone(),
                    });
                };
                granted.push((name.local.as_str(), decl.source.as_str(), sig));
            }
        }
        Ok(Self { granted, foreign })
    }

    /// What a local name was imported from, or `None` if this module imported
    /// no such name at all.
    fn resolve(&self, local: &str) -> Option<Bound<'a>> {
        if let Some((_, namespace, sig)) = self.granted.iter().find(|(name, _, _)| *name == local) {
            return Some(Bound::Host { namespace, sig });
        }
        self.foreign
            .iter()
            .find(|(name, _)| *name == local)
            .map(|(_, source)| Bound::Foreign(source))
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
        // (`fx.frobnicate`) is refused here rather than flattened into a callee
        // string, and a computed callee has no name to resolve at all.
        return Err(EffectError::Unresolved {
            attr: attr.to_string(),
            callee: expr_path(callee).unwrap_or_else(|| "a computed callee".to_string()),
        });
    };
    let (namespace, sig) = match scope.resolve(local.name.as_str()) {
        Some(Bound::Host { namespace, sig }) => (namespace, sig),
        // Imported, and from something with a source behind it. A Script is
        // compiled and a host import is granted (Rule 48), so this names no
        // signature - and saying "not imported" about a name the source plainly
        // imports would be wrong about the source.
        Some(Bound::Foreign(source)) => {
            return Err(EffectError::NotAHostImport {
                attr: attr.to_string(),
                callee: local.name.to_string(),
                source: source.to_string(),
            });
        }
        None => {
            return Err(EffectError::Unresolved {
                attr: attr.to_string(),
                callee: local.name.to_string(),
            });
        }
    };

    // **Arguments fill parameters positionally, and every parameter left
    // unfilled must be optional** ([`FieldDecl::optional`], which used to be
    // read by nobody here - see [`lower_arg`]'s doc for what that cost).
    //
    // Stated as "what is left over is optional" rather than as a count of
    // trailing optionals on purpose: it needs no rule about where an optional
    // parameter may appear in a signature. A signature declaring `(a?: T, b:
    // U)` handed one argument fills `a` and leaves `b` - required - unfilled,
    // so it is refused, which is the only honest answer for a positional call.
    let given = call.arguments.len();
    let required = sig.params.iter().filter(|p| !p.optional).count();
    if given > sig.params.len() || sig.params[given.min(sig.params.len())..].iter().any(|p| !p.optional) {
        return Err(EffectError::ArgCount {
            attr: attr.to_string(),
            effect: sig.name.clone(),
            declared: sig.params.len(),
            required,
            given,
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
        // The specifier the grant answered for - the other half of the
        // qualified name, so a reader downstream can tell two namespaces'
        // same-named effects apart (Rule 48).
        namespace: namespace.to_string(),
        // The RESOLVED name: an alias is spent here and never travels.
        name: sig.name.clone(),
        args,
    }))
}

/// Why an argument could not be lowered.
enum ArgFail {
    /// Neither a literal nor a binding path - a call, an arithmetic
    /// expression, a template literal, an arrow function, an object or array
    /// literal. An effect call is not an expression language (Rule 46a); a
    /// computation belongs in a Module.
    NotALiteral,
    /// A literal the declared parameter type cannot hold.
    WrongType,
}

/// Lower one argument **against its declared type**, so the signature decides
/// what a number becomes rather than the parser guessing (Rule 48).
///
/// Two forms are admitted, and they are two forms rather than one:
///
/// * a **literal**, checked against the declared [`TypeShape`];
/// * a **binding path** - `{id}`, `{props.user.name}` - lowered to
///   [`crate::dag::Expr::Get`], the same distinct first-class form
///   [`AttrValue::Binding`] is for an ordinary attribute
///   ([`EffectError::ArgNotALiteral`] records why that is not a widening of
///   Rule 46a).
///
/// **A binding is NOT type-checked, and cannot be here.** A path names
/// something in the runtime scope the element is rendered in - a list row's
/// own fields, the enclosing definition's props - and this function is handed
/// one module's text and a signature. Nothing in reach knows what `{id}` is.
/// So a binding is admitted against any declared parameter type and the check
/// that it *fits* belongs to whoever resolves the path, which is a different
/// layer and a later one. What is bought is that the path survives at all; what
/// is not bought is a promise about its type, and pretending otherwise would be
/// the more expensive of the two mistakes.
///
/// **Optionality is modelled by the caller**, not here: [`effect_attr`] fills
/// parameters positionally and refuses a call that leaves a non-optional one
/// unfilled, so this is only ever asked about an argument that was actually
/// written.
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
        // A BINDING PATH. `expr_path` recovers an identifier or a static member
        // chain and NOTHING else, which is exactly the line: a call
        // (`Id({id})`), a computed member (`row[i]`), an arithmetic expression
        // and a template literal all answer `None` here and stay refused.
        other => match expr_path(other) {
            Some(path) => Ok(E::Get { path }),
            None => Err(ArgFail::NotALiteral),
        },
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
    let mut errors = Vec::new();
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
            match convert_interface(decl) {
                Ok(iface) => interfaces.push(iface),
                // Every refused declaration is reported, not the first: an
                // author fixing one union should not have to re-run to find the
                // next, and the diagnostics above are a list for the same
                // reason.
                Err(why) => errors.push(format!("interface `{}`: {why}", decl.id.name)),
            }
        }
    }

    if errors.is_empty() {
        Ok(interfaces)
    } else {
        Err(errors)
    }
}

fn convert_interface(
    decl: &oxc_ast::ast::TSInterfaceDeclaration,
) -> Result<InterfaceDecl, String> {
    Ok(InterfaceDecl {
        name: decl.id.name.to_string(),
        fields: signatures_to_fields(&decl.body.body)?,
    })
}

fn signatures_to_fields(sigs: &[TSSignature]) -> Result<Vec<FieldDecl>, String> {
    let mut fields = Vec::new();
    for sig in sigs {
        if let TSSignature::TSPropertySignature(prop) = sig {
            let name = match &prop.key {
                PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                PropertyKey::StringLiteral(s) => s.value.to_string(),
                _ => continue,
            };
            let ty = match &prop.type_annotation {
                Some(ann) => {
                    type_shape(&ann.type_annotation).map_err(|why| format!("`{name}`: {why}"))?
                }
                None => TypeShape::String,
            };
            fields.push(FieldDecl {
                name,
                ty,
                optional: prop.optional,
            });
        }
    }
    Ok(fields)
}

/// Map a `TSType` onto the owned [`TypeShape`] vocabulary, or say why it has no
/// place in it.
///
/// **Fallible because one case cannot be answered**, not because the mapping is
/// risky: [`TypeShape`] has no sum type, so a union of two real types is a
/// declaration this vocabulary cannot hold. Everything else it does not model
/// becomes [`TypeShape::Named`] and stays a *reference* - which is honest,
/// because a named reference is exactly what an unmodelled type is - while a
/// discarded union member would be a declaration silently replaced by a
/// different one.
fn type_shape(ty: &TSType) -> Result<TypeShape, String> {
    Ok(match ty {
        TSType::TSBooleanKeyword(_) => TypeShape::Bool,
        // TS `number` lowers to F64 by default (see dag::TypeShape docs).
        TSType::TSNumberKeyword(_) => TypeShape::F64,
        TSType::TSBigIntKeyword(_) => TypeShape::S64,
        TSType::TSStringKeyword(_) => TypeShape::String,
        TSType::TSArrayType(arr) => TypeShape::List(Box::new(type_shape(&arr.element_type)?)),
        TSType::TSParenthesizedType(p) => type_shape(&p.type_annotation)?,
        TSType::TSTypeLiteral(lit) => TypeShape::Record(signatures_to_fields(&lit.members)?),
        TSType::TSUnionType(u) => union_shape(u)?,
        TSType::TSTypeReference(r) => reference_shape(r)?,
        // Anything else we don't model becomes an opaque named reference.
        _ => TypeShape::Named("unknown".to_string()),
    })
}

/// `T | undefined` / `T | null` -> `Option<T>`; **any other union is refused.**
///
/// # It used to collapse, and that is the defect this replaces
///
/// The rule was "other unions collapse to the first non-nullish member
/// (best-effort)", so `Id | Blank` parsed as `Id` and `Blank` disappeared with
/// no diagnostic anywhere. That is worse than unsupported: the author declared
/// a sum type, the parse answered with one arm of it, and every reader
/// downstream - the property sheet, the daemon's column planner, the seed
/// generator - saw a complete declaration that was not the one written.
///
/// [`TypeShape`] models products (`Record`) and options and has no sum, so
/// there is no arm to lower this to. Refusing says so at the one place that
/// knows; admitting it needs a `TypeShape` variant, and that is a serialized IR
/// change (see DRAFT_APP_PLAN.md's finding on it), not a parser change.
fn union_shape(u: &oxc_ast::ast::TSUnionType) -> Result<TypeShape, String> {
    let mut nullish = false;
    let mut members: Vec<&TSType> = Vec::new();
    for t in &u.types {
        match t {
            TSType::TSUndefinedKeyword(_) | TSType::TSNullKeyword(_) => nullish = true,
            other => members.push(other),
        }
    }
    match (members.len(), nullish) {
        (1, true) => Ok(TypeShape::Option(Box::new(type_shape(members[0])?))),
        (1, false) => type_shape(members[0]),
        // `undefined | null` alone: nullish and nothing to be optional ABOUT.
        (0, _) => Ok(TypeShape::Named("unknown".to_string())),
        (n, _) => Err(format!(
            "a union of {n} types is not modelled - the semantic AST has no sum type, only `T | undefined` / `T | null` (which is an Option)"
        )),
    }
}

/// `Array<T>` → `List<T>`; every other generic reference preserves its
/// constructor and all arguments as [`TypeShape::Apply`].
fn reference_shape(r: &oxc_ast::ast::TSTypeReference) -> Result<TypeShape, String> {
    let name = match &r.type_name {
        oxc_ast::ast::TSTypeName::IdentifierReference(id) => id.name.to_string(),
        _ => return Ok(TypeShape::Named("unknown".to_string())),
    };
    let Some(type_arguments) = &r.type_arguments else {
        return Ok(TypeShape::Named(name));
    };
    let args = type_arguments
        .params
        .iter()
        .map(type_shape)
        .collect::<Result<Vec<_>, _>>()?;
    if name == "Array" {
        if let Some(first) = args.first() {
            return Ok(TypeShape::List(Box::new(first.clone())));
        }
    }
    Ok(TypeShape::Apply {
        constructor: name,
        args,
    })
}

fn convert_element(jsx: &JSXElement, low: &Lowering) -> Result<Element, EffectError> {
    let tag = element_name(&jsx.opening_element.name);

    // `<List<Message> …>` — the opening tag's type arguments, through the same
    // `TypeShape` lowering an interface field's annotation takes.
    let type_args: Vec<TypeShape> = jsx
        .opening_element
        .type_arguments
        .as_ref()
        .map(|args| {
            args.params
                .iter()
                // A type ARGUMENT that this vocabulary cannot hold (a union -
                // see `union_shape`) becomes the same `unknown` reference every
                // other unmodelled type in this position becomes. It is not
                // silently accepted: a type argument resolves through
                // `declared_type_arg` against the source's own declarations, and
                // `highbay_data::compile::check_type_args` refuses one that
                // names an interface nothing declares - which `unknown` cannot
                // be. The interface-FIELD position has no such downstream check,
                // which is why that one is a hard refusal here.
                .map(|p| type_shape(p).unwrap_or(TypeShape::Named("unknown".to_string())))
                .collect()
        })
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
            effect_attr(&key, a.value.as_ref(), &low.scope)?
        } else {
            match &a.value {
                None => AttrValue::Bool(true),
                Some(JSXAttributeValue::StringLiteral(s)) => AttrValue::Str(s.value.to_string()),
                Some(JSXAttributeValue::ExpressionContainer(c)) => {
                    let Some(expr) = c.expression.as_expression() else {
                        return Err(EffectError::BindingSyntax {
                            attr: key,
                            message: "an empty expression container is unsupported".into(),
                        });
                    };
                    AttrValue::BindingExpr(lower_binding_expr(expr, &low.scope).map_err(|message| {
                        EffectError::BindingSyntax { attr: key.clone(), message }
                    })?)
                }
                _ => {
                    return Err(EffectError::BindingSyntax {
                        attr: key,
                        message: "the attribute value is not a supported expression".into(),
                    });
                }
            }
        };
        attrs.push((key, value));
    }

    let mut children = Vec::new();
    for child in &jsx.children {
        push_child(&mut children, child, low, Some(&tag))?;
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
    low: &Lowering,
    parent: Option<&str>,
) -> Result<(), EffectError> {
    match child {
        JSXChild::Element(e) => out.push(Node::Element(convert_element(e, low)?)),
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
                        // A substituted one COMPUTES, and the graph is one-way
                        // data flow: `{`hi ${name}`}` is a Module's job, or the
                        // `{{ }}` placeholder scan's ([`crate::template`]).
                        if !t.expressions.is_empty() || t.quasis.len() != 1 {
                            return Err(EffectError::UnreadableChild {
                                tag: parent.map(str::to_string),
                                form: "a template literal with a substitution".to_string(),
                            });
                        }
                        let Some(raw) = t.quasis[0].value.cooked.as_ref() else {
                            // `cooked` is None only for an escape sequence that
                            // has no value at all (`` `\u{}` ``), which is a
                            // string this parse cannot produce rather than one
                            // it declines to.
                            return Err(EffectError::UnreadableChild {
                                tag: parent.map(str::to_string),
                                form: "a template literal whose escape has no value".to_string(),
                            });
                        };
                        out.push(Node::Text(raw.to_string()));
                    }
                    Expression::ArrowFunctionExpression(arrow) => {
                        // A list render function is authored as
                        // `{(item) => <Item>...</Item>}`. The retained tree
                        // carries the returned JSX; the parameter remains
                        // available to its binding-valued props.
                        let Some(jsx) = arrow_root_jsx(arrow) else {
                            return Err(EffectError::UnreadableChild {
                                tag: parent.map(str::to_string),
                                form: "a render function that returns no element".to_string(),
                            });
                        };
                        out.push(Node::Element(convert_element(jsx, low)?));
                    }
                    other => {
                        // **The refusal, where a silent drop used to be.** An
                        // `expr_path` of `None` means this is not a binding
                        // path, and a child that is not one of the four node
                        // kinds is a finding for the surface to render - never
                        // a child the parse quietly forgets.
                        let Some(path) = expr_path(other) else {
                            return Err(EffectError::UnreadableChild {
                                tag: parent.map(str::to_string),
                                form: child_form(other).to_string(),
                            });
                        };
                        out.push(Node::Expr(path));
                    }
                }
            } else if let Some(comments) = &low.comments {
                // An expression container holding NO expression is how JSX
                // spells a comment among children: `{/* like this */}`. The
                // container is the only node oxc leaves behind, so the text is
                // read out of the source by span.
                for text in comments.within(c.span) {
                    out.push(Node::Comment(text.to_string()));
                }
            }
        }
        JSXChild::Fragment(frag) => {
            for c in &frag.children {
                push_child(out, c, low, parent)?;
            }
        }
        // `<A>{...kids}</A>`. The same fact as [`EffectError::SpreadAttribute`]
        // one position over: a spread's contents are not statically known, so
        // there is nothing to place and no way to check what was spread.
        JSXChild::Spread(_) => {
            return Err(EffectError::UnreadableChild {
                tag: parent.map(str::to_string),
                form: "a spread (`{...}`)".to_string(),
            });
        }
    }
    Ok(())
}

/// Name a JSX child expression by its **form**, for
/// [`EffectError::UnreadableChild`].
///
/// A form and not the source text: `push_child` is handed one expression
/// subtree and no source, and threading the whole file down to a refusal so it
/// could quote a span would put the source in every element conversion for the
/// sake of one message. The form is what a reader needs anyway - "an object
/// literal" says both what was written and why nothing can hold it, and it is
/// what the pinning tests name.
///
/// The fallback is deliberately vague and deliberately reachable: this match
/// covers what a JSX child plausibly holds, not all sixty-odd expression kinds,
/// and a form that is missing here is still refused.
///
/// # Why this is not [`binding_expr_kind`]
///
/// That one names the same expressions for the ATTRIBUTE position, and the two
/// positions do not refuse the same set: `v={a ?? b}`, `v={f(x)}` and
/// `v={{k: 1}}` all LOWER, because an attribute value is a whole
/// [`BindingExpr`], while the child position has only [`Node::Expr`] - a flat
/// path `String`. So the child vocabulary is the narrower one, and the fix for
/// the forms that deserve reading is in the NODE (an `Expr` that carries a
/// `BindingExpr`), not in [`expr_path`], which is also the callee reader.
/// Sharing one namer would hide that asymmetry behind a shared word list;
/// `binding_expr_kind`'s terse nouns ("call", "computed") also read as
/// `{kind} expressions are unsupported`, which is not this sentence.
fn child_form(expr: &Expression) -> &'static str {
    match expr {
        // Not a path only because `expr_path` does not unparen. A candidate for
        // READING rather than refusing - see `EffectError::UnreadableChild`.
        Expression::ParenthesizedExpression(_) => "a parenthesised expression",
        Expression::JSXElement(_) | Expression::JSXFragment(_) => {
            "an element inside an expression container"
        }
        Expression::ConditionalExpression(_) => "a conditional (`?:`)",
        Expression::LogicalExpression(e) => match e.operator {
            LogicalOperator::Coalesce => "a coalesce (`??`)",
            LogicalOperator::And => "a logical and (`&&`)",
            LogicalOperator::Or => "a logical or (`||`)",
        },
        Expression::BinaryExpression(_) => "a binary expression",
        Expression::UnaryExpression(_) => "a unary expression",
        Expression::CallExpression(_) => "a call",
        Expression::NewExpression(_) => "a constructor call",
        Expression::TaggedTemplateExpression(_) => "a tagged template",
        Expression::ComputedMemberExpression(_) => "a computed member (`a[b]`)",
        Expression::PrivateFieldExpression(_) => "a private member (`a.#b`)",
        // Rooted at a name it would BE a path, so reaching here means the base
        // is something else: `this.a`, `f().a`, `a[b].c`.
        Expression::StaticMemberExpression(_) => "a member chain not rooted at a name",
        Expression::ChainExpression(_) => "an optional chain (`?.`)",
        Expression::ObjectExpression(_) => "an object literal",
        Expression::ArrayExpression(_) => "an array literal",
        Expression::FunctionExpression(_) => "a function expression",
        Expression::AssignmentExpression(_) => "an assignment",
        Expression::SequenceExpression(_) => "a sequence (`,`)",
        Expression::AwaitExpression(_) => "an await",
        Expression::ThisExpression(_) => "`this`",
        Expression::NumericLiteral(_) => "a number literal",
        Expression::BigIntLiteral(_) => "a bigint literal",
        Expression::BooleanLiteral(_) => "a boolean literal",
        Expression::NullLiteral(_) => "`null`",
        Expression::RegExpLiteral(_) => "a regular expression",
        Expression::TSNonNullExpression(_) => "a non-null assertion (`!`)",
        Expression::TSAsExpression(_) => "an `as` cast",
        Expression::TSSatisfiesExpression(_) => "a `satisfies` expression",
        Expression::TSTypeAssertion(_) => "a type assertion",
        _ => "an expression this tree has no node for",
    }
}

/// **The public lowering seam: one TypeScript expression's TEXT to one node.**
///
/// The upward rung of the IR ladder, spelled as Rust's own fallible conversion
/// trait. It is the inverse of `impl From<&BindingExpr> for String`
/// ([`crate::emit`]) over the image of the parse - which is exactly the law
/// `codec_round_trip.rs` measures - and it is what lets a caller wrap an
/// expression as a pipeline stage without owning a document.
///
/// ```
/// use libtsx::dag::{BindingExpr, BindingLiteral};
///
/// let expr = BindingExpr::try_from(r#"props.label ?? "none""#).expect("lowers");
/// assert_eq!(
///     expr,
///     BindingExpr::Coalesce(vec![
///         BindingExpr::Path(vec!["props".into(), "label".into()]),
///         BindingExpr::Literal(BindingLiteral::String("none".into())),
///     ])
/// );
/// // And back, which is the pairing:
/// assert_eq!(String::from(&expr), r#"props.label ?? "none""#);
/// ```
///
/// # Why TEXT and not `&Expression`
///
/// [`lower_binding_expr`] below already takes ONE expression subtree root and
/// needs no document; what kept it private is that its parameter is
/// `&oxc_ast::Expression`, and **no `oxc_*` type appears in this crate's public
/// API** (the quarantine, PLAN §4 - it is what lets a consumer take `libtsx`
/// with `default-features = false` and never build oxc). So the public door
/// takes the fragment and parses it here. Callers who already hold an oxc AST
/// are all inside this crate and call the private function directly.
///
/// # What the wrapper does, and why there is one
///
/// The fragment is parsed as `(<text>\n);`. The parentheses are not cosmetic:
/// a bare `{a: 1}` in statement position is a BLOCK, so without them the one
/// expression that most needs this door could not come through it, and
/// `From`/`TryFrom` would not be inverse. [`unparen`] strips them again before
/// lowering, so the node is the same one the `<Probe v={...} />` document
/// spelling produces.
///
/// # The scope is empty, and that is the right scope
///
/// A bare fragment declares no imports, so there is nothing for an
/// [`EffectScope`] to hold. Nothing on this path consults one either: a scope
/// is read only by the `on..` EVENT grammar ([`effect_attr`]), which resolves a
/// callee to a granted host import. An ordinary binding expression carries its
/// callee as a name, and is the same node whatever a module imported.
#[allow(rustdoc::private_intra_doc_links)]
impl TryFrom<&str> for BindingExpr {
    type Error = ParseError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        // The trailing newline is what keeps a fragment ending in a `//`
        // comment from commenting out the closing parenthesis.
        let wrapped = format!("({source}\n);");
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, &wrapped, SourceType::tsx()).parse();
        if !ret.diagnostics.is_empty() {
            // Display, not Debug - see `extract_interfaces`; these strings are
            // user-facing.
            return Err(ParseError::Syntax(
                ret.diagnostics.into_iter().map(|e| e.to_string()).collect(),
            ));
        }
        let [Statement::ExpressionStatement(statement)] = &ret.program.body[..] else {
            return Err(ParseError::Binding(
                "the text is not a single expression".into(),
            ));
        };
        lower_binding_expr(
            &statement.expression,
            &EffectScope {
                granted: Vec::new(),
                foreign: Vec::new(),
            },
        )
        .map_err(ParseError::Binding)
    }
}

/// Lower an ordinary JSX expression container into the owned object-binding
/// vocabulary. This deliberately has no `Opaque` fallback: callers need an
/// explicit parser refusal when JavaScript would execute something the owned
/// graph cannot represent.
///
/// **It takes one expression subtree root and no document**, which is the whole
/// reason `impl TryFrom<&str> for BindingExpr` above can exist: the seam was
/// always here, and only its `&oxc_ast::Expression` parameter kept it private.
fn lower_binding_expr(expr: &Expression, _scope: &EffectScope) -> Result<BindingExpr, String> {
    use BindingExpr as B;
    let expr = unparen(expr);
    match expr {
        Expression::NullLiteral(_) => Ok(B::Literal(BindingLiteral::Null)),
        Expression::BooleanLiteral(v) => Ok(B::Literal(BindingLiteral::Bool(v.value))),
        Expression::NumericLiteral(v) => Ok(B::Literal(BindingLiteral::Number(v.value))),
        Expression::StringLiteral(v) => Ok(B::Literal(BindingLiteral::String(v.value.to_string()))),
        Expression::Identifier(_) => {
            let Some(path) = expr_path(expr) else {
                return Err("computed or private member paths are unsupported".into());
            };
            Ok(B::Path(path.split('.').map(str::to_owned).collect()))
        }
        Expression::StaticMemberExpression(_) => {
            // Rooted at an identifier, so the whole chain is ONE name and the
            // existing path spelling says everything: `props.value` must not
            // become a `Member` on a `Path`, or one source has two shapes.
            if let Some(path) = expr_path(expr) {
                return Ok(B::Path(path.split('.').map(str::to_owned).collect()));
            }
            // Otherwise the base is something other than a name -
            // `design().isAuthoring` being the case this exists for. Peel the
            // static segments off and lower whatever they hang from.
            let mut segments = Vec::new();
            let mut cursor = expr;
            while let Expression::StaticMemberExpression(member) = unparen(cursor) {
                if member.optional {
                    return Err("optional member access is unsupported".into());
                }
                segments.push(member.property.name.to_string());
                cursor = &member.object;
            }
            segments.reverse();
            let base = lower_binding_expr(cursor, _scope)?;
            // **The invariant above, as a POSTCONDITION of this arm.** A
            // redundant parenthesis is all it takes to arrive here with a base
            // that is itself a name: `(a).b` fails `expr_path` (the object is a
            // `ParenthesizedExpression`), peels to
            // `Member { base: Path(["a"]), path: ["b"] }`, emits `a.b` - which
            // is right - and re-parses through `expr_path` as
            // `Path(["a", "b"])`. One source, two shapes.
            //
            // Collapsing here rather than teaching `expr_path` to `unparen`
            // keeps the guarantee independent of the route taken into this arm:
            // whatever spelling reaches it, a chain whose base lowers to a name
            // leaves as ONE `Path`. (`expr_path` is also the child-node and
            // callee reader; widening what IT accepts would change what those
            // two capture, which is a different decision from this one.)
            if let B::Path(mut rooted) = base {
                rooted.extend(segments);
                return Ok(B::Path(rooted));
            }
            Ok(B::Member {
                base: Box::new(base),
                path: segments,
            })
        }
        Expression::ArrayExpression(array) => {
            let mut items = Vec::with_capacity(array.elements.len());
            for item in &array.elements {
                let Some(expr) = (match item {
                    oxc_ast::ast::ArrayExpressionElement::SpreadElement(_) => {
                        return Err("array spreads are unsupported".into())
                    }
                    oxc_ast::ast::ArrayExpressionElement::Elision(_) => {
                        return Err("sparse array holes are unsupported".into())
                    }
                    other => other.as_expression(),
                }) else {
                    return Err("array item is unsupported".into());
                };
                items.push(lower_binding_expr(expr, _scope)?);
            }
            Ok(B::Array(items))
        }
        Expression::ObjectExpression(object) => {
            let mut fields = Vec::with_capacity(object.properties.len());
            for property in &object.properties {
                let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(property) = property else {
                    return Err("object spreads are unsupported".into());
                };
                if property.computed {
                    return Err("computed object keys are unsupported".into());
                }
                if property.method || property.kind != oxc_ast::ast::PropertyKind::Init {
                    return Err("object methods and accessors are unsupported".into());
                }
                let name = match &property.key {
                    PropertyKey::StaticIdentifier(key) => key.name.to_string(),
                    PropertyKey::StringLiteral(key) => key.value.to_string(),
                    _ => return Err("computed object keys are unsupported".into()),
                };
                fields.push((name, lower_binding_expr(&property.value, _scope)?));
            }
            Ok(B::Record(fields))
        }
        Expression::CallExpression(call) => {
            let (namespace, name) = static_call_parts(&call.callee)?;
            if call.optional {
                return Err("optional calls are unsupported".into());
            }
            let type_args = call
                .type_arguments
                .as_ref()
                .map(|args| {
                    args.params
                        .iter()
                        .map(|arg| type_shape(arg).unwrap_or(TypeShape::Named("unknown".into())))
                        .collect()
                })
                .unwrap_or_default();
            let mut args = Vec::with_capacity(call.arguments.len());
            for arg in &call.arguments {
                let Some(arg) = arg.as_expression() else {
                    return Err("call argument spreads are unsupported".into());
                };
                args.push(lower_binding_expr(arg, _scope)?);
            }
            Ok(B::Call { namespace, name, type_args, args })
        }
        Expression::ArrowFunctionExpression(arrow) if arrow.r#async => {
            lower_async_arrow(arrow, _scope).map(B::Async)
        }
        // `x => x`. The expression-bodied, NON-async arrow - the other half of
        // the arrow capture, and what `xs.map(x => x)` needs now that no
        // variant reads a callee named `map` as a comprehension. oxc puts a
        // concise body in `body.statements` as a single expression statement
        // and sets `expression`, so that flag is what tells the two bodies
        // apart.
        Expression::ArrowFunctionExpression(arrow) if arrow.expression => {
            let params = arrow_params(arrow)?;
            let Some(Statement::ExpressionStatement(body)) = arrow.body.statements.first() else {
                return Err("an expression-bodied arrow must have an expression body".into());
            };
            Ok(B::Arrow {
                params,
                body: Box::new(lower_binding_expr(&body.expression, _scope)?),
            })
        }
        // `a ?? b`. Only the nullish operator: `||` and `&&` share oxc's
        // `LogicalExpression` and answer a DIFFERENT question (falsy vs
        // nullish), so they are refused here by name rather than collapsed
        // into one variant that would have to pick a meaning.
        Expression::LogicalExpression(logical)
            if logical.operator == LogicalOperator::Coalesce =>
        {
            let mut operands = Vec::new();
            flatten_coalesce(&logical.left, _scope, &mut operands)?;
            flatten_coalesce(&logical.right, _scope, &mut operands)?;
            Ok(B::Coalesce(operands))
        }
        Expression::ConditionalExpression(cond) => Ok(B::Cond {
            cond: Box::new(lower_binding_expr(&cond.test, _scope)?),
            then: Box::new(lower_binding_expr(&cond.consequent, _scope)?),
            other: Box::new(lower_binding_expr(&cond.alternate, _scope)?),
        }),
        // `===` and `==`, and ONLY those two of oxc's binary operators. The
        // negations are a second operator and stay refused (see
        // [`BindingExpr::Eq`]); every ordering comparison is refused for want
        // of anything asking for one.
        Expression::BinaryExpression(binary)
            if matches!(
                binary.operator,
                BinaryOperator::StrictEquality | BinaryOperator::Equality
            ) =>
        {
            Ok(B::Eq {
                left: Box::new(lower_binding_expr(&binary.left, _scope)?),
                right: Box::new(lower_binding_expr(&binary.right, _scope)?),
                strict: binary.operator == BinaryOperator::StrictEquality,
            })
        }
        Expression::ComputedMemberExpression(_) => {
            Err("computed member paths are unsupported".into())
        }
        // Every arrow the two positive arms above did not take: a non-async
        // arrow with a BLOCK body, and an `async` arrow with an expression
        // body (which `lower_async_arrow` refuses in its own words). Refused
        // by name at CAPTURE - a decision about which TypeScript the DAG
        // accepts, not a meaning layered onto it.
        Expression::ArrowFunctionExpression(_) => Err(
            "a block-bodied arrow must be `async`; a non-async arrow needs an expression body"
                .into(),
        ),
        Expression::AssignmentExpression(_)
        | Expression::UpdateExpression(_)
        | Expression::UnaryExpression(_)
        | Expression::BinaryExpression(_)
        | Expression::LogicalExpression(_)
        | Expression::AwaitExpression(_) => {
            Err(format!("{} expressions are unsupported", binding_expr_kind(expr)))
        }
        _ => Err(format!("{} expressions are unsupported", binding_expr_kind(expr))),
    }
}

/// Collect the operands of a `??` chain into one n-ary list.
///
/// `a ?? b ?? c` parses left-associatively, so the left operand of the outer
/// `??` is itself a `??`. Flattening it here is what makes
/// [`BindingExpr::Coalesce`] n-ary rather than a nest of pairs, so one source
/// has exactly one representation.
///
/// **A PARENTHESISED `??` is not flattened.** `(a ?? b) ?? c` reaches this
/// through [`unparen`] with the same meaning as `a ?? b ?? c`, and flattening
/// it is correct - `??` is associative on its own. What must not be flattened
/// across is a different operator, and that cannot arrive here: TS is a syntax
/// error on `a ?? b || c`, and the parenthesised form `(a || b) ?? c` is a
/// `LogicalExpression` with the `||` operator, which the recursion refuses.
fn flatten_coalesce(
    expr: &Expression,
    scope: &EffectScope,
    out: &mut Vec<BindingExpr>,
) -> Result<(), String> {
    if let Expression::LogicalExpression(logical) = unparen(expr) {
        if logical.operator == LogicalOperator::Coalesce {
            flatten_coalesce(&logical.left, scope, out)?;
            flatten_coalesce(&logical.right, scope, out)?;
            return Ok(());
        }
    }
    out.push(lower_binding_expr(expr, scope)?);
    Ok(())
}

/// The `(namespace, name)` a call's callee spells.
///
/// A qualified callee (`ns.fn()`) yields both halves. An **unqualified** one
/// (`design()`) yields an empty namespace and the bare name: that is what the
/// author wrote, and writing down a namespace nobody spelled would be an
/// invention this layer is not allowed to make. Consumers that require a
/// namespace still refuse the empty one - `highbay_objects`' checked plan does,
/// by name - so widening the capture widens no consumer's grammar.
fn static_call_parts(expr: &Expression) -> Result<(String, String), String> {
    match unparen(expr) {
        Expression::Identifier(id) => Ok((String::new(), id.name.to_string())),
        Expression::StaticMemberExpression(member) => {
            let Some(namespace) = expr_path(&member.object) else {
                return Err("computed call callees are unsupported".into());
            };
            Ok((namespace, member.property.name.to_string()))
        }
        Expression::ComputedMemberExpression(_) => Err("computed call callees are unsupported".into()),
        _ => Err("call callee must be a static name or qualified member".into()),
    }
}

fn lower_async_arrow(
    arrow: &ArrowFunctionExpression,
    scope: &EffectScope,
) -> Result<BlockArrow, String> {
    if arrow.expression {
        return Err("async expression-bodied arrows are unsupported; use an explicit block".into());
    }
    Ok(BlockArrow {
        params: arrow_params(arrow)?,
        body: lower_block_body(&arrow.body.statements, scope)?,
    })
}

/// The parameter list both arrow captures share.
///
/// One function because the two spellings must describe their parameters
/// IDENTICALLY: a reader that had to ask which arrow a `BindingParam` came from
/// would be reading a difference the author never wrote. An unannotated
/// parameter arrives as `TypeShape::Named("unknown")`, which is what
/// [`type_shape`] answers for an unmodelled annotation too, so the emitted
/// `: unknown` re-parses to the same shape.
fn arrow_params(arrow: &ArrowFunctionExpression) -> Result<Vec<BindingParam>, String> {
    if arrow.params.rest.is_some() {
        return Err("arrow rest parameters are unsupported".into());
    }
    let mut params = Vec::with_capacity(arrow.params.items.len());
    for param in &arrow.params.items {
        let Some(id) = param.pattern.get_binding_identifier() else {
            return Err("arrow parameters must be simple identifiers".into());
        };
        let ty = param
            .type_annotation
            .as_ref()
            .map(|ty| type_shape(&ty.type_annotation))
            .transpose()
            .map_err(|e| format!("arrow parameter type is unsupported: {e}"))?
            .unwrap_or(TypeShape::Named("unknown".into()));
        params.push(BindingParam { name: id.name.to_string(), ty });
    }
    Ok(params)
}

fn lower_block_body(
    statements: &[Statement],
    scope: &EffectScope,
) -> Result<Vec<BlockStmt>, String> {
    let mut out = Vec::new();
    for statement in statements {
        out.extend(lower_block_statement(statement, scope)?);
    }
    Ok(out)
}

fn lower_block_statement(
    statement: &Statement,
    scope: &EffectScope,
) -> Result<Vec<BlockStmt>, String> {
    use BlockStmt as S;
    match statement {
        Statement::BlockStatement(block) => lower_block_body(&block.body, scope),
        Statement::VariableDeclaration(decl) => {
            let mut out = Vec::with_capacity(decl.declarations.len());
            for declarator in &decl.declarations {
                let Some(id) = declarator.id.get_binding_identifier() else {
                    return Err("destructuring declarations are unsupported".into());
                };
                let Some(init) = declarator.init.as_ref() else {
                    return Err("uninitialized declarations are unsupported".into());
                };
                let value = unparen(init);
                if let Expression::AwaitExpression(awaited) = value {
                    out.push(S::Await {
                        slot: Some(id.name.to_string()),
                        awaitable: lower_binding_expr(&awaited.argument, scope)?,
                    });
                } else {
                    out.push(S::Let {
                        slot: id.name.to_string(),
                        value: lower_binding_expr(value, scope)?,
                    });
                }
            }
            Ok(out)
        }
        Statement::ExpressionStatement(statement) => {
            let Expression::AwaitExpression(awaited) = unparen(&statement.expression) else {
                return Err("expression statements are unsupported in async blocks".into());
            };
            Ok(vec![S::Await {
                slot: None,
                awaitable: lower_binding_expr(&awaited.argument, scope)?,
            }])
        }
        Statement::ReturnStatement(statement) => {
            let Some(value) = statement.argument.as_ref() else {
                return Err("empty returns are unsupported in async blocks".into());
            };
            Ok(vec![S::Return(lower_binding_expr(value, scope)?)])
        }
        Statement::IfStatement(statement) => {
            let then_branch = lower_block_statement(&statement.consequent, scope)?;
            let else_branch = statement
                .alternate
                .as_ref()
                .map(|alternate| lower_block_statement(alternate, scope))
                .transpose()?
                .unwrap_or_default();
            Ok(vec![S::If {
                condition: lower_binding_expr(&statement.test, scope)?,
                then_branch,
                else_branch,
            }])
        }
        Statement::TryStatement(statement) => {
            let Some(handler) = statement.handler.as_ref() else {
                return Err("try statements require a catch clause".into());
            };
            if statement.finalizer.is_some() {
                return Err("try/finally is unsupported in async blocks".into());
            }
            let Some(param) = handler.param.as_ref() else {
                return Err("catch clauses require an error identifier".into());
            };
            let Some(id) = param.pattern.get_binding_identifier() else {
                return Err("catch parameters must be simple identifiers".into());
            };
            Ok(vec![S::Try {
                body: lower_block_body(&statement.block.body, scope)?,
                error_slot: id.name.to_string(),
                catch: lower_block_body(&handler.body.body, scope)?,
            }])
        }
        _ => Err(format!(
            "{} statements are unsupported in async blocks",
            effect_statement_kind(statement)
        )),
    }
}

/// What to CALL the thing being refused.
///
/// It exists so a refusal names the form the author wrote. It had a hole worth
/// recording: `||`, `&&` and `? :` fell through to the catch-all and produced
/// `"expression expressions are unsupported"`, which says nothing and reads as
/// a bug in the message rather than a verdict on the source. `??` and `? :` are
/// lowered now; `||` and `&&` are still refused, and they are refused BY NAME -
/// so an author who writes one is told which operator this vocabulary declines
/// and is not left guessing whether the parser understood the line at all.
fn binding_expr_kind(expr: &Expression) -> &'static str {
    match expr {
        Expression::AssignmentExpression(_) | Expression::UpdateExpression(_) => "mutation",
        Expression::CallExpression(_) => "call",
        Expression::ArrowFunctionExpression(_) => "function",
        Expression::ComputedMemberExpression(_) => "computed",
        Expression::NewExpression(_) => "constructor",
        // Reached only for `||`/`&&`: the `??` operator has a positive arm and
        // never gets here.
        Expression::LogicalExpression(logical) => match logical.operator {
            LogicalOperator::Or => "`||`",
            LogicalOperator::And => "`&&`",
            LogicalOperator::Coalesce => "`??`",
        },
        Expression::ConditionalExpression(_) => "conditional",
        // Reached for every binary operator EXCEPT `===`/`==`, which have a
        // positive arm. The negations are named individually because "we
        // support equality" and "we refused your `!==`" are one keystroke
        // apart, and an author who is told only "binary-operator" will read
        // that as the equality support being absent.
        Expression::BinaryExpression(binary) => match binary.operator {
            BinaryOperator::Inequality => "`!=`",
            BinaryOperator::StrictInequality => "`!==`",
            BinaryOperator::LessThan
            | BinaryOperator::LessEqualThan
            | BinaryOperator::GreaterThan
            | BinaryOperator::GreaterEqualThan => "ordering-comparison",
            _ => "binary-operator",
        },
        Expression::UnaryExpression(_) => "unary-operator",
        Expression::AwaitExpression(_) => "await",
        _ => "expression",
    }
}

fn effect_statement_kind(statement: &Statement) -> &'static str {
    match statement {
        Statement::ForStatement(_)
        | Statement::ForInStatement(_)
        | Statement::ForOfStatement(_)
        | Statement::WhileStatement(_)
        | Statement::DoWhileStatement(_) => "loop",
        Statement::ExpressionStatement(_) => "expression",
        _ => "statement",
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
        assert_eq!(
            list.attrs[0].1,
            AttrValue::BindingExpr(BindingExpr::Path(vec!["props".into(), "items".into()]))
        );
        assert_eq!(
            list.attrs[1].1,
            AttrValue::BindingExpr(BindingExpr::Literal(BindingLiteral::Number(3.0)))
        );
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
        assert_eq!(
            list.attr("value"),
            Some(&AttrValue::BindingExpr(BindingExpr::Path(vec!["chatFeed".into()])))
        );
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

    #[test]
    fn generic_references_preserve_all_arguments_except_array_list_sugar() {
        let interfaces = extract_interfaces(
            r#"
                interface Props {
                    result: Result<User, Error>;
                    selected: PartialData<User, string>;
                    values: Array<Result<User, Error>>;
                }
            "#,
        )
        .expect("generic references parse");
        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::Apply {
                constructor: "Result".into(),
                args: vec![
                    TypeShape::Named("User".into()),
                    TypeShape::Named("Error".into()),
                ],
            }
        );
        assert_eq!(
            interfaces[0].fields[1].ty,
            TypeShape::Apply {
                constructor: "PartialData".into(),
                args: vec![TypeShape::Named("User".into()), TypeShape::String],
            }
        );
        assert_eq!(
            interfaces[0].fields[2].ty,
            TypeShape::List(Box::new(TypeShape::Apply {
                constructor: "Result".into(),
                args: vec![
                    TypeShape::Named("User".into()),
                    TypeShape::Named("Error".into()),
                ],
            }))
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

    /// **A union of two real types is REFUSED, not collapsed** - the defect
    /// [`union_shape`] documents. `Id | Blank` used to parse as `Id`, so a
    /// declared sum type reached every reader as one arm of itself with no
    /// diagnostic anywhere.
    ///
    /// The refusal names the interface and the field, because the whole point is
    /// that an author can find it.
    #[test]
    fn a_union_of_two_real_types_is_refused_rather_than_collapsed() {
        let errors = extract_interfaces("interface Route { record: Id | Blank; }")
            .expect_err("a sum type has no place in this vocabulary");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("Route"), "{errors:?}");
        assert!(errors[0].contains("record"), "{errors:?}");
        assert!(errors[0].contains("sum type"), "{errors:?}");
        assert!(errors[0].is_ascii(), "{errors:?}");

        // ...and the nullish forms are untouched: they are an Option, which this
        // vocabulary does model.
        for src in [
            "interface P { a: string | undefined; }",
            "interface P { a: string | null; }",
            "interface P { a?: string | undefined | null; }",
        ] {
            let ifaces = extract_interfaces(src).expect(src);
            assert_eq!(
                ifaces[0].fields[0].ty,
                TypeShape::Option(Box::new(TypeShape::String)),
                "{src}",
            );
        }

        // A nested union is refused through the containers too - a record field
        // and a list element are the two ways one hides.
        for src in [
            "interface P { a: { b: Id | Blank }; }",
            "interface P { a: (Id | Blank)[]; }",
        ] {
            assert!(extract_interfaces(src).is_err(), "{src}");
        }

        // Every refused declaration is reported, not just the first.
        let errors = extract_interfaces(
            "interface A { x: Id | Blank; }\ninterface B { y: Id | Blank; }",
        )
        .expect_err("two refusals");
        assert_eq!(errors.len(), 2, "{errors:?}");
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
        assert_eq!(
            list.attr("value"),
            Some(&AttrValue::BindingExpr(BindingExpr::Path(vec!["libraryFeed".into()])))
        );

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
        assert_eq!(
            app_el.attr("depth"),
            Some(&AttrValue::BindingExpr(BindingExpr::Literal(BindingLiteral::Number(2.0))))
        );
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
    fn ordinary_expression_containers_lower_structurally() {
        let doc = parse_tsx(
            r#"<Thing
                text={"hello"}
                path={props.user.name}
                values={[true, props.count, null]}
                record={{first: 1, second: "two"}}
            />"#,
        )
        .expect("object binding expressions parse");
        let Node::Element(thing) = &doc.root_nodes[0] else {
            panic!("expected element")
        };
        assert_eq!(
            thing.attr("text"),
            Some(&AttrValue::BindingExpr(BindingExpr::Literal(
                BindingLiteral::String("hello".into())
            )))
        );
        assert_eq!(
            thing.attr("path"),
            Some(&AttrValue::BindingExpr(BindingExpr::Path(vec![
                "props".into(),
                "user".into(),
                "name".into()
            ])))
        );
        assert_eq!(
            thing.attr("values"),
            Some(&AttrValue::BindingExpr(BindingExpr::Array(vec![
                BindingExpr::Literal(BindingLiteral::Bool(true)),
                BindingExpr::Path(vec!["props".into(), "count".into()]),
                BindingExpr::Literal(BindingLiteral::Null),
            ])))
        );
        assert_eq!(
            thing.attr("record"),
            Some(&AttrValue::BindingExpr(BindingExpr::Record(vec![
                (
                    "first".into(),
                    BindingExpr::Literal(BindingLiteral::Number(1.0))
                ),
                (
                    "second".into(),
                    BindingExpr::Literal(BindingLiteral::String("two".into()))
                ),
            ])))
        );
    }

    #[test]
    fn qualified_generic_calls_keep_namespace_name_and_type_arguments() {
        let doc = parse_tsx(
            r#"<Thing value={objects.make<User, Error>(props.id, "fallback")} />"#,
        )
        .expect("qualified generic call parses");
        let Node::Element(thing) = &doc.root_nodes[0] else {
            panic!("expected element")
        };
        assert_eq!(
            thing.attr("value"),
            Some(&AttrValue::BindingExpr(BindingExpr::Call {
                namespace: "objects".into(),
                name: "make".into(),
                type_args: vec![
                    TypeShape::Named("User".into()),
                    TypeShape::Named("Error".into())
                ],
                args: vec![
                    BindingExpr::Path(vec!["props".into(), "id".into()]),
                    BindingExpr::Literal(BindingLiteral::String("fallback".into())),
                ],
            }))
        );
    }

    #[test]
    fn unsupported_binding_expressions_are_typed_refusals() {
        for (source, expected) in [
            (r#"<Thing value={objects[method]}/>"#, "computed"),
            (r#"<Thing value={count++}/>"#, "mutation"),
            (r#"<Thing value={{...props}}/>"#, "spreads"),
        ] {
            let Err(ParseError::Effect(EffectError::BindingSyntax { message, .. })) =
                ParseCtx::default().parse_tsx(source)
            else {
                panic!("expected BindingSyntax for {source}");
            };
            assert!(message.contains(expected), "{message:?}");
        }
    }

    /// Read one attribute's lowered binding expression, or panic.
    fn binding_of(source: &str) -> BindingExpr {
        let doc = parse_tsx(source).expect("parse");
        let Node::Element(element) = &doc.root_nodes[0] else {
            panic!("expected an element");
        };
        let Some(AttrValue::BindingExpr(expr)) = element.attr("value") else {
            panic!("expected a lowered binding expression on `value`");
        };
        expr.clone()
    }

    fn path(segments: &[&str]) -> BindingExpr {
        BindingExpr::Path(segments.iter().map(|s| (*s).to_string()).collect())
    }

    /// `??` lowers, and a CHAIN of it is one n-ary node rather than a nest.
    ///
    /// The flattening is the claim worth pinning: `a ?? b ?? c` is
    /// left-associative in TS, so it arrives as a `??` whose left operand is
    /// another `??`, and a lowering that kept that shape would give one source
    /// two representations - the nested one and the flat one a later author
    /// could equally mean - leaving every consumer to normalise. There is one
    /// shape, and this says which.
    #[test]
    fn the_nullish_operator_lowers_and_a_chain_is_one_n_ary_node() {
        assert_eq!(
            binding_of(r#"<Thing value={props.placeholder ?? props.value}/>"#),
            BindingExpr::Coalesce(vec![
                path(&["props", "placeholder"]),
                path(&["props", "value"]),
            ])
        );
        assert_eq!(
            binding_of(r#"<Thing value={a ?? b ?? c}/>"#),
            BindingExpr::Coalesce(vec![path(&["a"]), path(&["b"]), path(&["c"])]),
            "a chain flattens rather than nesting"
        );
        assert_eq!(
            binding_of(r#"<Thing value={(a ?? b) ?? c}/>"#),
            BindingExpr::Coalesce(vec![path(&["a"]), path(&["b"]), path(&["c"])]),
            "parentheses around a `??` say nothing a re-parse could tell apart"
        );
    }

    /// `||` and `&&` stay refused, and the refusal NAMES the operator.
    ///
    /// They share oxc's `LogicalExpression` with `??`, so the positive arm had
    /// to be written on the operator rather than the node kind - and that is
    /// what makes this test load-bearing rather than a restatement: an arm
    /// matching the node would have silently lowered `a || b` as a coalesce,
    /// answering a falsy test with a nullish one.
    ///
    /// Before this change all three produced `"expression expressions are
    /// unsupported"`, which named nothing.
    #[test]
    fn the_other_logical_operators_are_refused_by_name() {
        for (source, expected) in [
            (r#"<Thing value={a || b}/>"#, "`||`"),
            (r#"<Thing value={a && b}/>"#, "`&&`"),
            (r#"<Thing value={a > b}/>"#, "ordering-comparison"),
            (r#"<Thing value={a != b}/>"#, "`!=`"),
            (r#"<Thing value={a !== b}/>"#, "`!==`"),
            (r#"<Thing value={a + b}/>"#, "binary-operator"),
            (r#"<Thing value={!a}/>"#, "unary-operator"),
        ] {
            let Err(ParseError::Effect(EffectError::BindingSyntax { message, .. })) =
                ParseCtx::default().parse_tsx(source)
            else {
                panic!("expected BindingSyntax for {source}");
            };
            assert!(
                message.contains(expected),
                "{source} should name its operator, said {message:?}"
            );
        }
    }

    /// The ternary lowers, including nested in its own branches.
    #[test]
    fn the_conditional_expression_lowers() {
        assert_eq!(
            binding_of(r#"<Thing value={props.on ? props.a : props.b}/>"#),
            BindingExpr::Cond {
                cond: Box::new(path(&["props", "on"])),
                then: Box::new(path(&["props", "a"])),
                other: Box::new(path(&["props", "b"])),
            }
        );
        let nested = binding_of(r#"<Thing value={a ? b : c ? d : e}/>"#);
        let BindingExpr::Cond { other, .. } = &nested else {
            panic!("expected a conditional");
        };
        assert!(
            matches!(**other, BindingExpr::Cond { .. }),
            "the else branch carries the nested conditional"
        );
    }

    /// `design().isAuthoring` - the shape `Member` exists for.
    ///
    /// Two facts in one: an UNQUALIFIED callee is captured with an empty
    /// namespace (writing down a namespace nobody spelled would be an
    /// invention), and the member access hangs off the call rather than being
    /// folded into its name.
    #[test]
    fn a_member_chain_on_a_call_result_lowers_to_member() {
        assert_eq!(
            binding_of(r#"<Thing value={design().isAuthoring}/>"#),
            BindingExpr::Member {
                base: Box::new(BindingExpr::Call {
                    namespace: String::new(),
                    name: "design".into(),
                    type_args: vec![],
                    args: vec![],
                }),
                path: vec!["isAuthoring".into()],
            }
        );
        assert_eq!(
            binding_of(r#"<Thing value={design().avatar.sm.box}/>"#),
            BindingExpr::Member {
                base: Box::new(BindingExpr::Call {
                    namespace: String::new(),
                    name: "design".into(),
                    type_args: vec![],
                    args: vec![],
                }),
                path: vec!["avatar".into(), "sm".into(), "box".into()],
            },
            "a deep chain is ONE member node, in source order"
        );
    }

    /// **A plain dotted path did NOT become a `Member`.**
    ///
    /// The regression this guards is the whole reason `Member` is restricted to
    /// non-identifier bases: `props.value` has a perfectly good spelling
    /// already, every reader in the workspace knows it, and a lowering that
    /// re-expressed it as a member access on a path would have changed the
    /// meaning of every attribute in the shipped corpus while every test that
    /// only checks *rendering* stayed green.
    #[test]
    fn an_identifier_rooted_chain_is_still_a_path() {
        assert_eq!(
            binding_of(r#"<Thing value={props.user.name}/>"#),
            path(&["props", "user", "name"])
        );
        assert_eq!(binding_of(r#"<Thing value={items}/>"#), path(&["items"]));
    }

    /// `===` and `==` both lower, and WHICH ONE was written survives.
    ///
    /// The pin is the last clause. Folding the two operators together is the
    /// tempting simplification - this vocabulary has no coercion, so they
    /// cannot currently disagree - and it is the one that cannot be undone:
    /// once `==` has been recorded as `===` the source is gone. `strict` costs
    /// a bool and keeps the question open for whoever needs it.
    #[test]
    fn both_equality_operators_lower_and_keep_their_spelling() {
        assert_eq!(
            binding_of(r#"<Thing value={design().fidelity === "lofi"}/>"#),
            BindingExpr::Eq {
                left: Box::new(BindingExpr::Member {
                    base: Box::new(BindingExpr::Call {
                        namespace: String::new(),
                        name: "design".into(),
                        type_args: vec![],
                        args: vec![],
                    }),
                    path: vec!["fidelity".into()],
                }),
                right: Box::new(BindingExpr::Literal(BindingLiteral::String("lofi".into()))),
                strict: true,
            }
        );
        assert_eq!(
            binding_of(r#"<Thing value={props.kind == "row"}/>"#),
            BindingExpr::Eq {
                left: Box::new(path(&["props", "kind"])),
                right: Box::new(BindingExpr::Literal(BindingLiteral::String("row".into()))),
                strict: false,
            },
            "a loose equality is recorded as a loose equality"
        );
    }

    /// Computed access stays refused wherever it sits in a chain.
    #[test]
    fn computed_access_is_still_refused_under_a_call() {
        let Err(ParseError::Effect(EffectError::BindingSyntax { message, .. })) =
            ParseCtx::default().parse_tsx(r#"<Thing value={design()[key]}/>"#)
        else {
            panic!("expected BindingSyntax");
        };
        assert!(message.contains("computed"), "{message:?}");
    }

    /// **Emit -> parse gives the tree back**, which is what the conservative
    /// parenthesisation is for.
    ///
    /// The emitter has no precedence model, so the cases that matter are the
    /// ones where splicing text would re-associate: a `??` inside a ternary
    /// branch, a ternary inside a ternary's condition, and a `??` operand that
    /// is itself a ternary. Each is emitted with parentheses it does not
    /// strictly need in every position; the property being asserted is not
    /// "the text is minimal" but "the text means what the tree said".
    #[test]
    fn the_new_operators_survive_a_round_trip_through_emit() {
        for source in [
            r#"<Thing value={design().isAuthoring ? (props.authoringPlaceholder ?? props.value) : props.value}/>"#,
            r#"<Thing value={a ?? b ?? c}/>"#,
            r#"<Thing value={(a ? b : c) ? d : e}/>"#,
            r#"<Thing value={(a ? b : c) ?? d}/>"#,
            r#"<Thing value={a ? (b ? c : d) : e}/>"#,
            r#"<Thing value={design().avatar.sm.box ?? 8}/>"#,
            r#"<Thing value={design().fidelity === "lofi" ? a : b}/>"#,
            r#"<Thing value={(a === b) === c}/>"#,
            r#"<Thing value={props.kind == "row"}/>"#,
            r#"<Thing value={(a ?? b) === c}/>"#,
            // The expression-bodied arrow, and the four shapes whose
            // parenthesisation the emitter has to get right: an operator
            // BESIDE an arrow (the arrow's body would swallow it), an operator
            // INSIDE one (it must not be lifted out), an object-literal body
            // (a bare `{` opens a block), and an arrow in a ternary branch.
            r#"<Thing value={xs.map(x => x.label)}/>"#,
            r#"<Thing value={xs.map((x: Item) => ({id: x.id, label: x.label}))}/>"#,
            r#"<Thing value={(x => x) ?? fallback}/>"#,
            r#"<Thing value={xs.map(x => x.label ?? "none")}/>"#,
            r#"<Thing value={props.ready ? (x => x) : (x => x.other)}/>"#,
            r#"<Thing value={() => 1}/>"#,
            r#"<Thing value={(a, b) => a === b}/>"#,
            // `await` binds tighter than every operator in this vocabulary, so
            // its operand is the one splice site inside a block that is not
            // already delimited by a bracket, a comma or a keyword.
            r#"<Thing value={async () => { const r = await (a ?? b); return r; }}/>"#,
            r#"<Thing value={async () => { await (props.ready ? a : b); return null; }}/>"#,
            r#"<Thing value={async () => { const r = await ((x) => x); return r; }}/>"#,
        ] {
            let first = parse_tsx(source).expect("parse");
            let emitted = crate::emit::emit_tsx_document(&first);
            let second = parse_tsx(&emitted)
                .unwrap_or_else(|e| panic!("re-parse of {emitted:?} failed: {e:?}"));
            assert_eq!(
                first, second,
                "{source} emitted as {emitted:?} and re-parsed differently"
            );
        }
    }

    /// **`xs.map(x => x.label)` is a CALL with an arrow argument**, which is
    /// what TypeScript says it is. Nothing here reads the callee's name: a
    /// consumer that wants a comprehension out of `map` applies that reading
    /// itself, and one that wants `filter` or `flatMap` needs no new variant.
    #[test]
    fn a_map_call_is_a_call_whose_argument_is_an_arrow() {
        let doc = parse_tsx(r#"<Thing value={xs.map((x: Item) => x.label)}/>"#)
            .expect("a map call parses");
        let Node::Element(thing) = &doc.root_nodes[0] else {
            panic!("expected element")
        };
        let AttrValue::BindingExpr(BindingExpr::Call { namespace, name, args, .. }) =
            thing.attr("value").expect("value")
        else {
            panic!("expected a call");
        };
        assert_eq!((namespace.as_str(), name.as_str()), ("xs", "map"));
        let [BindingExpr::Arrow { params, body }] = args.as_slice() else {
            panic!("expected one arrow argument, got {args:?}");
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "x");
        assert_eq!(params[0].ty, TypeShape::Named("Item".into()));
        assert_eq!(
            **body,
            BindingExpr::Path(vec!["x".into(), "label".into()])
        );
    }

    /// An arrow parameter with no annotation is `unknown`, and the emitter's
    /// `: unknown` re-parses to the same shape - which is what lets
    /// [`the_new_operators_survive_a_round_trip_through_emit`] hold for arrows
    /// at all.
    #[test]
    fn an_unannotated_arrow_parameter_is_named_unknown() {
        let doc = parse_tsx(r#"<Thing value={x => x}/>"#).expect("a bare arrow parses");
        let Node::Element(thing) = &doc.root_nodes[0] else {
            panic!("expected element")
        };
        let AttrValue::BindingExpr(BindingExpr::Arrow { params, .. }) =
            thing.attr("value").expect("value")
        else {
            panic!("expected an arrow");
        };
        assert_eq!(params[0].ty, TypeShape::Named("unknown".into()));

        let emitted = crate::emit::emit_tsx_document(&doc);
        assert!(
            emitted.contains("(x: unknown) => x"),
            "the emitter annotates and parenthesises: {emitted}"
        );
    }

    /// **The known-bad twin for the arrow's parenthesisation.** Each source
    /// below is emitted with a pair of parentheses that a reader would call
    /// redundant; strip either one and the text re-parses as a DIFFERENT tree,
    /// which the round-trip test above would then catch. This one names the
    /// characters, so the reason they are there survives someone tidying them.
    #[test]
    fn the_arrows_parentheses_are_load_bearing() {
        // An object-literal body: without the pair, `=> {` opens a block and
        // TypeScript reads `id:` as a label, not as a field.
        let doc = parse_tsx(r#"<Thing value={xs.map(x => ({id: x.id}))}/>"#).expect("parse");
        let emitted = crate::emit::emit_tsx_document(&doc);
        assert!(
            emitted.contains("=> ({id: x.id})"),
            "an object body keeps its parentheses: {emitted}"
        );
        assert!(
            parse_tsx(&emitted.replace("({id: x.id})", "{id: x.id}")).is_err(),
            "without them the text is not even this vocabulary any more"
        );

        // An arrow beside an operator: without the pair, `??` lands INSIDE the
        // arrow body and the coalesce disappears.
        let doc = parse_tsx(r#"<Thing value={(x => x) ?? fallback}/>"#).expect("parse");
        let emitted = crate::emit::emit_tsx_document(&doc);
        assert!(
            emitted.contains("((x: unknown) => x) ?? fallback"),
            "an arrow operand keeps its parentheses: {emitted}"
        );
        let stripped = emitted.replace("((x: unknown) => x) ??", "(x: unknown) => x ??");
        let reparsed = parse_tsx(&stripped).expect("the stripped text still parses");
        assert_ne!(
            doc, reparsed,
            "stripping the parentheses must change the tree, or they prove nothing"
        );
    }

    /// **`await`'s operand keeps its parentheses**, and this names the
    /// characters so a reader tidying them meets the reason.
    ///
    /// `await` takes a UnaryExpression - tighter than every operator this
    /// vocabulary captures - so a bare splice turns `await (a ?? b)` into
    /// `await a ?? b`, which is `(await a) ?? b`. That is not a different
    /// spelling of the same tree; it is not even in this vocabulary, and the
    /// re-parse refuses it outright. The bug predates the arrow, which is why
    /// this test exists separately from the arrow's.
    #[test]
    fn awaits_operand_keeps_its_parentheses() {
        let doc = parse_tsx(
            r#"<Thing value={async () => { const r = await (a ?? b); return r; }}/>"#,
        )
        .expect("parse");
        let emitted = crate::emit::emit_tsx_document(&doc);
        assert!(
            emitted.contains("await (a ?? b)"),
            "the operand keeps its parentheses: {emitted}"
        );
        let stripped = emitted.replace("await (a ?? b)", "await a ?? b");
        assert!(
            parse_tsx(&stripped).is_err(),
            "without them the text is not this vocabulary any more, which is \
             what makes the parentheses load-bearing rather than tidy"
        );
    }

    /// The two arrow shapes NEITHER capture admits are refused at capture, by
    /// name. Both are subset decisions - which TypeScript the DAG accepts -
    /// and neither may become a silent `Opaque`.
    #[test]
    fn the_arrow_shapes_outside_the_subset_are_refused_by_name() {
        let block = parse_tsx(r#"<Thing value={x => { return x; }}/>"#)
            .expect_err("a non-async block-bodied arrow is refused");
        assert!(
            format!("{block:?}").contains("must be `async`"),
            "refused for the wrong reason: {block:?}"
        );
        let async_concise = parse_tsx(r#"<Thing value={async x => x}/>"#)
            .expect_err("an async expression-bodied arrow is refused");
        assert!(
            format!("{async_concise:?}").contains("async expression-bodied arrows"),
            "refused for the wrong reason: {async_concise:?}"
        );
    }

    /// **Every child form the tree cannot hold is refused BY NAME.**
    ///
    /// Each of these used to parse to an element with NO children and no error
    /// at all - the child the author wrote was read, found unrepresentable, and
    /// dropped. The census that preceded this change found the authored corpus
    /// (69 `{...}` children across 17 `.tsx` files) drops none of them, so
    /// nothing here is a shape anything ships; what it defends is that the next
    /// one is a finding rather than a hole.
    ///
    /// The forms are named individually because "refuse it" and "read it" are
    /// different answers and some of these deserve the second - see
    /// [`child_form`] for which, and why the fix would be in [`Node::Expr`].
    ///
    /// **Probe**: restoring the old `if let Some(path) = expr_path(other)` (the
    /// silent drop) fails this test and
    /// [`an_optional_chain_is_refused_in_both_positions`], and leaves
    /// [`the_child_forms_the_tree_holds_are_untouched`] green - which is the
    /// pair working as intended, one gate per direction.
    #[test]
    fn every_unreadable_child_form_is_refused_by_name() {
        let cases: &[(&str, &str)] = &[
            // The parenthesised path that surfaced this: `expr_path` does not
            // unparen, so one redundant pair of brackets erased the child.
            ("<A>{(props.a.b)}</A>", "a parenthesised expression"),
            ("<A>{props.ready ? props.a : props.b}</A>", "a conditional (`?:`)"),
            ("<A>{props.a ?? props.b}</A>", "a coalesce (`??`)"),
            ("<A>{props.a && props.b}</A>", "a logical and (`&&`)"),
            ("<A>{props.a || props.b}</A>", "a logical or (`||`)"),
            ("<A>{props.a === props.b}</A>", "a binary expression"),
            ("<A>{!props.a}</A>", "a unary expression"),
            ("<A>{f(props.a)}</A>", "a call"),
            ("<A>{props.items.map((i) => (<Item />))}</A>", "a call"),
            ("<A>{new Thing()}</A>", "a constructor call"),
            ("<A>{props.items[0]}</A>", "a computed member (`a[b]`)"),
            ("<A>{this.a}</A>", "a member chain not rooted at a name"),
            ("<A>{props?.a}</A>", "an optional chain (`?.`)"),
            ("<A>{{a: 1}}</A>", "an object literal"),
            // The spelling that cost this repo real content: a `{{ }}`
            // placeholder written without its quotes is a JS object literal.
            ("<A>{{greeting}}</A>", "an object literal"),
            ("<A>{[1, 2]}</A>", "an array literal"),
            ("<A>{7}</A>", "a number literal"),
            ("<A>{true}</A>", "a boolean literal"),
            ("<A>{null}</A>", "`null`"),
            ("<A>{props.a!}</A>", "a non-null assertion (`!`)"),
            ("<A>{props.a as string}</A>", "an `as` cast"),
            ("<A>{<B />}</A>", "an element inside an expression container"),
            // Not the catch-all arm, the same defect: three more child shapes
            // that were read and thrown away in silence.
            ("<A>{`hi ${props.a}`}</A>", "a template literal with a substitution"),
            ("<A>{() => 1}</A>", "a render function that returns no element"),
            ("<A>{...props.kids}</A>", "a spread (`{...}`)"),
        ];
        for (source, form) in cases {
            let error = ParseCtx::default()
                .parse_tsx(source)
                .expect_err(&format!("{source} must not parse to a silent drop"));
            assert_eq!(
                error,
                ParseError::Effect(EffectError::UnreadableChild {
                    tag: Some("A".to_string()),
                    form: (*form).to_string()
                }),
                "{source} was refused as something else"
            );
        }

        // A top-level FRAGMENT has no tag to name, and says so rather than
        // inventing one. (A fragment nested inside an element reports that
        // element: the tag is the nearest one a reader can search for.)
        assert_eq!(
            ParseCtx::default().parse_tsx("<>{f(x)}</>"),
            Err(ParseError::Effect(EffectError::UnreadableChild {
                tag: None,
                form: "a call".to_string()
            }))
        );
        assert_eq!(
            ParseCtx::default().parse_tsx("<A><>{f(x)}</></A>"),
            Err(ParseError::Effect(EffectError::UnreadableChild {
                tag: Some("A".to_string()),
                form: "a call".to_string()
            }))
        );
    }

    /// **The known-bad twin of the refusal above**: every child form the tree
    /// DOES have a node for still reads, and reads to the same node it always
    /// did. A refusal that swallowed one of these would pass the test above
    /// while gutting every authored screen.
    ///
    /// **Probe**: refusing unconditionally in the catch-all arm (a refusal one
    /// step too broad) fails this test, `libhbui`'s
    /// `a_child_the_tree_cannot_hold_is_refused_by_name` and its whole authored
    /// corpus walk, while the refusal test above still passes.
    #[test]
    fn the_child_forms_the_tree_holds_are_untouched() {
        let doc = ParseCtx::builder()
            .retain_comments()
            .build()
            .parse_tsx(
                r#"<A>
                    plain text
                    {"a string child"}
                    {`a plain template`}
                    {props.user.name}
                    {/* a comment */}
                    <B />
                    {(item) => (<Item value={item} />)}
                </A>"#,
            )
            .expect("every readable child form parses");
        let Node::Element(a) = &doc.root_nodes[0] else {
            panic!("expected an element")
        };
        assert_eq!(
            a.children,
            vec![
                Node::Text("plain text".into()),
                Node::Text("a string child".into()),
                Node::Text("a plain template".into()),
                Node::Expr("props.user.name".into()),
                Node::Comment("/* a comment */".into()),
                Node::Element(Element {
                    tag: "B".into(),
                    type_args: Vec::new(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                }),
                Node::Element(Element {
                    tag: "Item".into(),
                    type_args: Vec::new(),
                    attrs: vec![(
                        "value".into(),
                        AttrValue::BindingExpr(BindingExpr::Path(vec!["item".into()])),
                    )],
                    children: Vec::new(),
                }),
            ]
        );
        // A comment child on the PUBLISH path (comments not retained) is still
        // the one container that legitimately contributes no node - it is not a
        // dropped expression, it is a comment this parse was not asked to keep.
        let published = parse_tsx(r#"<A>{/* a comment */}</A>"#).expect("parses");
        let Node::Element(a) = &published.root_nodes[0] else {
            panic!("expected an element")
        };
        assert!(a.children.is_empty());
    }

    /// **`a?.b` is refused in BOTH positions, and `expr_path` is why that
    /// matters.**
    ///
    /// [`expr_path`] reads `member.property` and never looks at
    /// `member.optional`, so an optional chain arriving there would lower to
    /// the same `Path(["a", "b"])` as `a.b` - two different sources, one shape,
    /// silently. It cannot arrive today because oxc wraps an optional chain in
    /// a `ChainExpression`, which both readers below refuse before any member
    /// is peeled; the peel loop in `lower_binding_expr`'s member arm checks
    /// `optional` and `expr_path` above it does not, and this pins that the
    /// split stays unreachable.
    ///
    /// This asserts the refusals, NOT the lowering: if a later wave teaches
    /// either reader to see through a `ChainExpression`, this test fails and
    /// `expr_path` has to answer for `optional` first.
    #[test]
    fn an_optional_chain_is_refused_in_both_positions() {
        assert_eq!(
            ParseCtx::default().parse_tsx("<A>{a?.b}</A>"),
            Err(ParseError::Effect(EffectError::UnreadableChild {
                tag: Some("A".to_string()),
                form: "an optional chain (`?.`)".to_string()
            })),
            "an optional chain in child position"
        );
        let attribute = ParseCtx::default()
            .parse_tsx("<A v={a?.b} />")
            .expect_err("an optional chain in attribute position");
        assert_eq!(
            attribute,
            ParseError::Effect(EffectError::BindingSyntax {
                attr: "v".to_string(),
                message: "expression expressions are unsupported".to_string(),
            }),
            "an optional chain must not lower as if the `?` were not written"
        );
        // The twin: the same text WITHOUT the question mark is the path both
        // readers do produce, which is exactly what a silent acceptance of the
        // optional form would have looked like.
        let doc = ParseCtx::default().parse_tsx("<A>{a.b}</A>").expect("parses");
        let Node::Element(a) = &doc.root_nodes[0] else {
            panic!("expected an element")
        };
        assert_eq!(a.children, vec![Node::Expr("a.b".into())]);
    }

    #[test]
    fn explicit_async_blocks_lower_supported_effect_statements() {
        let doc = parse_tsx(
            r#"<Thing value={async (input: User) => {
                const loaded = await objects.load<User>(input.id);
                if (input.ready) {
                    return loaded;
                }
                return null;
            }} />"#,
        )
        .expect("async block parses");
        let Node::Element(thing) = &doc.root_nodes[0] else {
            panic!("expected element")
        };
        let AttrValue::BindingExpr(BindingExpr::Async(program)) =
            thing.attr("value").expect("value")
        else {
            panic!("expected async binding");
        };
        assert_eq!(program.params[0].name, "input");
        assert_eq!(program.params[0].ty, TypeShape::Named("User".into()));
        assert!(matches!(&program.body[1], BlockStmt::If { .. }));
        assert!(matches!(
            &program.body[2],
            BlockStmt::Return(BindingExpr::Literal(BindingLiteral::Null))
        ));
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
}
