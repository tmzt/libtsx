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
//! * **Event bindings are produced here, not inferred later.** An
//!   [`is_event_binding`] attribute's value is parsed as one call resolving to
//!   a granted host import and emitted as an [`AttrValue::BindingExpr`]
//!   carrying a [`BindingExpr::Call`]; there is no pass that later decides an
//!   [`AttrValue::Opaque`] was really a call (LIBHBUI_PLAN Rules 46a, 48).
//! * **Configuration is a context, not a second entry point.** What a load
//!   offers a source is [`ParseCtx`], built through [`ParseCtx::builder`] and
//!   passed to whichever parse entry point the caller needs (Rule 49).

use crate::dag::{
    AttrValue, BindingExpr, BindingParam, EffectError, BlockArrow, BlockStmt,
    Element, FieldDecl, FuncSig, ImportDecl, ImportKind, ImportName, InterfaceDecl,
    LiteralValue, Node, ObjectSymbol, ParserHost, PropertyAccessor, Resolution, TsxDocument,
    TypeShape, is_event_binding, is_host_namespace,
};
use std::sync::Arc;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, ExportDefaultDeclarationKind, Expression, ImportDeclarationSpecifier,
    BinaryOperator, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXElementName, LogicalOperator, ModuleExportName, PropertyKey, Statement, TSSignature, TSType,
    UnaryOperator,
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
    /// An event binding or a host import that cannot mean what it says
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
/// could not express an imported call at all.
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
/// The default offers **nothing**, which is the honest one: a source calling a
/// host import it was never given has named a capability it does not have.
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

/// The provider [`ParseCtxBuilder::enable_host_imports`] installs: the surface is
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
    pub fn enable_host_imports(mut self) -> Self {
        self.host.get_or_insert_with(|| Arc::new(NoGrants));
        self
    }

    /// Set the embedding's [`ParserHost`], **offering the surface** in the same
    /// step.
    ///
    /// A provider is the stronger statement, so it implies
    /// [`Self::enable_host_imports`] rather than needing it: a caller that sets a
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
    /// grants, and becomes an [`AttrValue::BindingExpr`] carrying a
    /// [`BindingExpr::Call`]. Nothing about that is
    /// deferred: an attribute that announced an event binding and cannot carry
    /// one is refused **here**, with a typed [`EffectError`], rather than surviving as
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

        // Pass 0: the import edges, and the import scope they open. This runs
        // BEFORE any element is converted, because a callee resolves against
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
            scope: ImportScope::build(&imports, self)?,
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
    /// exists: every file here parses in *this* context, so a host import the
    /// embedding offered is available in a screen file. Its predecessor was a
    /// third free function that granted nothing - not by decision, but because
    /// nobody extended it - and the multi-file path could not express an
    /// imported call at all (Rule 49).
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
    scope: ImportScope<'a>,
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

// --- the import scope (LIBHBUI_PLAN Rules 46a, 48) ----------------------------

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
/// by something it was never imported from - so [`ImportScope::resolve`] walks
/// the declaration to a specifier first and only then to what the provider
/// declares under it.
///
/// **A name imported from a Script or a package is remembered too**
/// ([`ImportScope::foreign`]). Only a host namespace can supply a callable, but
/// "you never imported that" and "you imported that from something compiled"
/// are different facts about the source, and the provider is what lets the
/// parse tell them apart.
struct ImportScope<'a> {
    /// `(local, namespace, signature)` for every name bound from a granted host
    /// namespace, in source order. The namespace is kept because it is half the
    /// call's identity (Rule 48) and the only half a callee cannot recover:
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
    /// A Script or a package - imported, and unable to supply a callable.
    Foreign(&'a str),
}

impl<'a> ImportScope<'a> {
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

/// Lower an `on..` attribute's value into a [`BindingExpr::Call`], or refuse it
/// (Rules 46a, 48).
///
/// The attribute already announced itself as an event binding, so every exit
/// from here is either an [`AttrValue::BindingExpr`] carrying a call or an
/// error - there is deliberately no path that yields [`AttrValue::Opaque`].
///
/// # The call is checked HERE and carried as an ordinary call
///
/// The value used to become an `AttrValue::ImportedCall` - three fields, no
/// type arguments - and now becomes the same call the ordinary binding
/// vocabulary already had a shape for. **What is checked did not move**: the
/// callee still resolves through the module's import chain to a granted host
/// import, the arguments are still filled positionally against the declared
/// [`FuncSig`], and every refusal below is the one it always was. What changed
/// is that the result has somewhere to put `frobnicate<Sprocket>("x")`'s type
/// argument.
///
/// The two vocabularies for an argument met here too: an argument was lowered
/// to a `crate::dag::Expr` (the handler-body vocabulary) and is now lowered to
/// a [`BindingExpr`] like every other attribute value. The narrowing is
/// unchanged and still [`LiteralValue::narrow`]'s - what goes away is the
/// transcription onto a second literal ladder.
fn imported_call_attr(
    attr: &str,
    value: Option<&JSXAttributeValue>,
    scope: &ImportScope,
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

    // The type arguments the callee was written with, read through the SAME
    // mapping the ordinary call arm and `<List<Message>>` use - one type
    // vocabulary, not a third one for this position.
    let type_args: Vec<TypeShape> = call
        .type_arguments
        .as_ref()
        .map(|args| {
            args.params
                .iter()
                .map(|arg| type_shape(arg).unwrap_or(TypeShape::Named("unknown".into())))
                .collect()
        })
        .unwrap_or_default();

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
            .and_then(|expr| lower_arg(unparen(expr), &param.ty, scope))
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

    Ok(AttrValue::BindingExpr(BindingExpr::Call {
        // The specifier the grant answered for - the other half of the
        // qualified name, so a reader downstream can tell two namespaces'
        // same-named imports apart (Rule 48). A host specifier carries
        // `host:`, which no member path can spell, so a granted call is
        // distinguishable from `a.b()` by the namespace alone
        // ([`is_host_namespace`]).
        namespace: namespace.to_string(),
        // The RESOLVED name: an alias is spent here and never travels.
        name: sig.name.clone(),
        // **The whole reason this is a `Call`.** Read exactly as the ordinary
        // call arm reads them, so one authored `<T>` has one capture wherever
        // it is written. Nothing checks them against the signature: a
        // `FuncSig` declares no type parameters, so there is nothing here to
        // check against, and refusing what cannot be checked would refuse the
        // spelling this change exists to admit.
        type_args,
        args,
    }))
}

/// Why an argument could not be lowered.
enum ArgFail {
    /// Not one of the three admissible forms - see [`admissible_arg`], which
    /// is where the line is drawn and argued. An arithmetic expression, a
    /// template literal, an arrow function, an object or array literal, a
    /// spread and an optional call all land here. An imported call is not an
    /// expression language (Rule 46a); a computation belongs in a Module.
    NotALiteral,
    /// A literal the declared parameter type cannot hold.
    WrongType,
}

/// Lower one argument **against its declared type**, so the signature decides
/// what a number becomes rather than the parser guessing (Rule 48).
///
/// **The declared type chooses the width, and a literal that cannot be
/// represented in it exactly is [`ArgFail::WrongType`] - never a rounded,
/// truncated or wrapped value.** That is this function's contract, not a
/// property of one arm: it is what stops a silently rounded id or navigation
/// destination from being written into the graph and only failing much later,
/// somewhere with no view of the source text that caused it. Refusing costs an
/// error at parse time, which is the cheap end of that trade.
///
/// Three forms are admitted, and they are three forms rather than one:
///
/// * a **literal**, checked against the declared [`TypeShape`];
/// * a **binding path** - `{id}`, `{props.user.name}` - lowered to
///   [`BindingExpr::Path`], the same distinct first-class form
///   [`AttrValue::Binding`] is for an ordinary attribute
///   ([`EffectError::ArgNotALiteral`] records why that is not a widening of
///   Rule 46a);
/// * a **symbol projection or a value in call form** - `listItem()`,
///   `uiComponent().props.pane`, `pane("name")` - admitted by SHAPE, through
///   [`admissible_arg`], which holds the whole of that decision.
///
/// # Everything but a literal is lowered by the ORDINARY lowering
///
/// The non-literal arm hands the expression to [`lower_binding_expr`] - the
/// one an ordinary attribute binding takes - and then asks whether the shape
/// that came back is admissible. It does NOT lower these forms itself, and
/// that is the point: a second lowering written beside the first is how one
/// authored text comes to have two captures, and the whole value of the
/// widening is that `onTap={f(uiComponent().props.pane)}` and
/// `value={uiComponent().props.pane}` produce the SAME expression, so
/// everything downstream that already walks one walks the other. The
/// admission is a predicate over the RESULT, never a parallel parse.
///
/// A literal keeps its own arm above it because a literal - and only a
/// literal - is checked against the declared parameter type, which
/// [`lower_binding_expr`] knows nothing about.
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
/// **Optionality is modelled by the caller**, not here: [`imported_call_attr`] fills
/// parameters positionally and refuses a call that leaves a non-optional one
/// unfilled, so this is only ever asked about an argument that was actually
/// written.
fn lower_arg(
    expr: &Expression,
    declared: &TypeShape,
    scope: &ImportScope,
) -> Result<BindingExpr, ArgFail> {
    match expr {
        // CAPTURE, then NARROW - and the narrowing rules live on
        // [`LiteralValue::narrow`], not here. The widths of the two
        // vocabularies meet in exactly one function (see [`LiteralValue`]); a
        // second copy of "does this fit?" written at this call site is how the
        // binding side and the event side would come to disagree about 2^53
        // without either being wrong on its own terms.
        Expression::StringLiteral(s) => {
            lower_literal_arg(LiteralValue::String(s.value.to_string()), declared)
        }
        Expression::BooleanLiteral(b) => lower_literal_arg(LiteralValue::Bool(b.value), declared),
        Expression::NumericLiteral(n) => lower_literal_arg(numeric_literal(n), declared),
        // EVERYTHING ELSE, through the door that already exists. This used to
        // read `expr_path`, which recovers an identifier or a static member
        // chain and nothing else - so a chain ROOTED AT A CALL
        // (`uiComponent().props.pane`) answered `None` and was refused,
        // although the very same text in an ordinary attribute lowers
        // perfectly well one function along. The refusal was an accident of
        // which reader this position happened to call, not a rule anybody
        // wrote down; the rule is [`admissible_arg`], and it is applied to
        // what the ordinary lowering produced.
        //
        // A lowering REFUSAL and an inadmissible SHAPE are one answer here:
        // both mean this text is not an effect argument, and
        // [`ArgFail::NotALiteral`] is the caller's only word for that. The
        // string `lower_binding_expr` returns is dropped rather than wrapped
        // because the caller turns this into [`EffectError::ArgNotALiteral`],
        // which carries the attribute, the effect and the index - the three
        // things an author needs to find the argument.
        other => {
            let lowered = lower_binding_expr(other, scope).map_err(|_| ArgFail::NotALiteral)?;
            admissible_arg(&lowered)
                .then_some(lowered)
                .ok_or(ArgFail::NotALiteral)
        }
    }
}

/// **May this expression stand in an effect argument?** - the whole of the
/// line Rule 46a draws, in one recursive predicate.
///
/// An argument is a **literal**, a **binding path**, a **symbol**, a **member
/// read on one of those**, or a **call whose own arguments are admissible by
/// this same rule**. Nothing else.
///
/// # Why a call is not a widening of Rule 46a
///
/// Rule 46a is *an imported call is not an expression language; a computation
/// belongs in a Module*, and it is intact. What is admitted here is not a
/// computation but a **value in call form**: `pane("name")` names a
/// constructor and the members it fills, in the one spelling TypeScript has
/// for that. Nothing is evaluated, nothing is combined, and no operator is
/// admitted - `a + b`, a template literal, `a ?? b`, `a === b`, `!flag` and a
/// conditional are each refused below BY NAME, which is where the rule's line
/// actually lives. A reader who arrives asking "did calls just become
/// expressions?" is asking the right question, and the answer is that the
/// operators are still the boundary; an argument that CONSTRUCTS is on the
/// naming side of it, exactly as a path is.
///
/// The precedent is the caller's own: a binding path was refused here for the
/// same reason - it looked like structure - until it was noticed that a path
/// NAMES rather than computes. This is that same observation one shape along.
///
/// # Why no name is checked, and why that is Rule 52 holding
///
/// **This predicate never looks at a callee's name.** `pane`, `listItem`,
/// `frobnicate` and a misspelling of any of them are one case here, because
/// [`ObjectSymbol::intern`] is total and *"which identifiers matter is the
/// consumer's question"* - the reason already written at the ordinary call arm
/// of [`lower_binding_expr`]. A callee that resolves to no declared symbol is
/// refused by the consumer, at the door that holds the registry; a parser that
/// refused it here would be holding a second copy of that registry, and the
/// two would disagree the first time one of them grew.
///
/// The same follows for a nested literal: `pane("name")`'s `"name"` is NOT
/// narrowed against anything, because the thing that declares what `pane`'s
/// members are is not in this crate. Only the OUTER argument has a declared
/// type here, and only it is checked ([`lower_arg`]).
///
/// # A member read is admitted at whatever it is rooted in
///
/// `MemberOf` recurses into its base rather than testing that base against a
/// list, so `uiComponent().props.pane` is admitted for the reason its base
/// `uiComponent()` is - which arrives as [`BindingExpr::SymbolValue`], a bare
/// identifier call being a symbol and not a `Call`. It also admits
/// `pane("a").name`, a projection out of a constructed value, which nothing
/// has asked for yet; refusing that would need an extra clause saying "a
/// member chain may not be rooted in a call with arguments", and there is no
/// sentence to write under it. One recursion, no arity rule - the same
/// argument the symbol registry makes about `listOf()` and `listOf(a, b)`
/// being one symbol.
///
/// # Exhaustive with no wildcard, deliberately
///
/// A variant appended to [`BindingExpr`] arrives here as a compile error and
/// gets a decision, rather than being silently admitted (a `_ => true` tail)
/// or silently refused (a `_ => false` one). The refusals are listed by name
/// for the same reason the admissions are: this is the only place that says
/// which TypeScript an effect argument may be written in.
fn admissible_arg(expr: &BindingExpr) -> bool {
    use BindingExpr as B;
    match expr {
        // NAMES a value: a literal is one, a path names one in the runtime
        // scope, a symbol names one in the consumer's.
        B::Literal(_) | B::Path(_) | B::SymbolValue(_) => true,
        // A read ON a value, admitted exactly when what it reads from is.
        B::MemberOf(base, _) => admissible_arg(base),
        // A value in CALL form, and the recursion that makes this rule a rule
        // rather than a list: a constructor's arguments are arguments.
        B::Call { args, .. } => args.iter().all(admissible_arg),
        // Everything that COMBINES or DEFERS. `a ?? b`, `c ? x : y`, `a == b`,
        // `a != b`, `!flag` are operators; `[..]` and `{..}` are structure an
        // effect signature has no parameter for; an arrow - async or not - is
        // a computation with a body, which is the Module case Rule 46a names.
        // `null` is refused because an argument that is nothing is an argument
        // not written, and the caller already has a word for that (an
        // unfilled optional parameter).
        B::Array(_)
        | B::Record(_)
        | B::Async(_)
        | B::Coalesce(_)
        | B::Cond { .. }
        | B::Eq { .. }
        | B::Arrow { .. }
        | B::Null
        | B::Not(_)
        | B::Ne { .. } => false,
    }
}

/// **The literal a numeric token captures as, at CAPTURE width.**
///
/// An INTEGRAL literal is [`LiteralValue::Int64`] and a DECIMAL one is
/// [`LiteralValue::Float64`], decided from the token as it was WRITTEN. Tim,
/// 2026-08-23: *"it's safe to have integers as Int64, then map to floats only
/// if the props require a float. 64 bit ints/floats are cheap for us and
/// provide the most compatibility."*
///
/// **Nothing narrows here.** [`LiteralValue::Int32`] and
/// [`LiteralValue::Float32`] are what a field DECLARING them produces, through
/// [`LiteralValue::narrow`] at lowering, and they are deliberately not
/// reachable from a bare token: a parser that guessed a width from the value
/// would make `1` a different literal from `10_000_000_000` for reasons the
/// source does not state, and the declaration - the only thing that knows -
/// would arrive too late to disagree.
///
/// # The integer is read from the SOURCE TEXT, not from oxc's `value`
///
/// `NumericLiteral::value` is an `f64` - the lexer has already rounded, because
/// that is what a JavaScript number IS - so a token past 2^53 arrives there as
/// a DIFFERENT integer with nothing to say so. `4605617453661332513` becomes
/// `4605617453661332480` before this function is called. Reading `raw` recovers
/// what the author typed, which is the only reading that makes an `Int64`
/// capture worth having: an integer that is silently a nearby integer is the
/// same defect as one that is silently a float.
///
/// `raw` is `None` only for a node the parser did not build; the value's own
/// fractional part is the fallback there, and a hand-built node has no source
/// text to be faithful to.
fn numeric_literal(literal: &oxc_ast::ast::NumericLiteral) -> LiteralValue {
    let Some(raw) = literal.raw.as_ref() else {
        return match whole_f64(literal.value) {
            Some(value) => LiteralValue::Int64(value),
            None => LiteralValue::Float64(literal.value),
        };
    };
    let text = raw.replace('_', "");
    if text.contains(['.', 'e', 'E']) {
        return LiteralValue::Float64(literal.value);
    }
    // A non-decimal radix is still an integral token; TS spells the three with
    // a prefix, and each is exact in the source however wide it is.
    let parsed = match text.get(..2).map(str::to_ascii_lowercase).as_deref() {
        Some("0x") => i64::from_str_radix(&text[2..], 16),
        Some("0o") => i64::from_str_radix(&text[2..], 8),
        Some("0b") => i64::from_str_radix(&text[2..], 2),
        _ => text.parse::<i64>(),
    };
    match parsed {
        Ok(value) => LiteralValue::Int64(value),
        // Past i64 an integral token is not an integer this vocabulary can
        // hold, and rounding it into one is the silent narrowing this change
        // exists to refuse. It stays the float the lexer already made of it -
        // lossy, but VISIBLY so, and typed as what it is.
        Err(_) => LiteralValue::Float64(literal.value),
    }
}

/// `value` as an `i64` when it is exactly one - the fallback for a numeric node
/// with no source text.
fn whole_f64(value: f64) -> Option<i64> {
    (value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0).then_some(value as i64)
}

/// One captured literal, **narrowed to `declared`** - [`ArgFail::WrongType`]
/// when it does not fit exactly.
///
/// # It used to transcribe onto a second literal ladder, and no longer does
///
/// An event argument was a `crate::dag::Expr`, whose `LitBool`/`LitS32`/
/// `LitS64`/`LitF32`/`LitF64`/`LitStr` are the same ladder [`LiteralValue`]
/// writes as `Bool`/`Int32`/`Int64`/`Float32`/`Float64`/`String`. This function
/// was the one place the two were transcribed. With the event lowering
/// re-pointed at [`BindingExpr`] there is one ladder, so the narrowing is the
/// whole of the work and the transcription is gone rather than moved.
///
/// The narrowing itself is untouched and still lives on
/// [`LiteralValue::narrow`]: it is where the widths of the vocabularies meet,
/// and a second copy of "does this fit?" written at a call site is how two
/// sides come to disagree about 2^53 without either being wrong on its own
/// terms.
///
/// `U32`/`U64` cannot arrive: neither has an authored spelling (see
/// [`TypeShape`]), so no TS parameter can declare one and
/// [`LiteralValue::narrow`] answers `None` for both - refused, which is the
/// correct answer while nothing can be declared as them.
fn lower_literal_arg(
    captured: LiteralValue,
    declared: &TypeShape,
) -> Result<BindingExpr, ArgFail> {
    captured
        .narrow(declared)
        .map(BindingExpr::Literal)
        .ok_or(ArgFail::WrongType)
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

/// One `interface`, INCLUDING its `extends` clause.
///
/// # The clause used to be dropped, silently
///
/// This function read `decl.body` and nothing else, so
/// `interface P extends Omit<B, "a"> {}` produced
/// `InterfaceDecl { extends: Vec::new(), name: "P", fields: [] }` - no fields, no diagnostic. See [`TypeShape::Extends`] for what
/// that cost.
///
/// Each heritage entry is lowered through the SAME [`type_shape`] a field's
/// annotation takes, so `extends Omit<..>` and `field: Omit<..>` cannot
/// disagree about what `Omit` means, and a clause this vocabulary cannot hold
/// is refused here rather than silently discarded.
fn convert_interface(
    decl: &oxc_ast::ast::TSInterfaceDeclaration,
) -> Result<InterfaceDecl, String> {
    let name = decl.id.name.to_string();
    let extends = decl
        .extends
        .iter()
        .map(|heritage| {
            Ok(TypeShape::Extends {
                base: Box::new(heritage_shape(heritage).map_err(|why| format!("`{name}`: {why}"))?),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(InterfaceDecl {
        id: None,
        version: None,
        name,
        fields: signatures_to_fields(&decl.body.body)?,
        extends,
    })
}

/// One heritage entry as a type expression.
///
/// oxc models `extends Foo<Bar>` as an EXPRESSION plus separate type arguments
/// rather than as a `TSType`, so it cannot be handed straight to [`type_shape`].
/// This rebuilds the two halves into the shape that spelling means - which is
/// exactly what [`reference_shape`] answers for the same text written in field
/// position, `Omit`/`Pick` handling included.
fn heritage_shape(
    heritage: &oxc_ast::ast::TSInterfaceHeritage,
) -> Result<TypeShape, String> {
    let Expression::Identifier(id) = &heritage.expression else {
        return Err(
            "an `extends` entry is a named type; a qualified or computed one is not modelled"
                .to_string(),
        );
    };
    let name = id.name.to_string();
    let Some(type_arguments) = &heritage.type_arguments else {
        return Ok(TypeShape::Named(name));
    };
    // Through the same lowering the field position takes, by handing it the
    // same two pieces `reference_shape` reads.
    key_operator_or_apply(&name, &type_arguments.params)
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
/// **Fallible because some forms cannot be answered**, not because the mapping
/// is risky. It used to be fallible for exactly one reason - the vocabulary had
/// no sum type, so a union of two real types could not be held - and
/// [`TypeShape::Union`] has since answered that one. What remains fallible is
/// the literal forms with no carrier ([`literal_shape`]): a bigint literal type
/// and a template literal type.
///
/// Everything else it does not model becomes [`TypeShape::Named`] and stays a
/// *reference* - which is honest, because a named reference is exactly what an
/// unmodelled type is - while a discarded union member, or a literal flattened
/// into `Named("unknown")`, is a declaration silently replaced by a different
/// one.
fn type_shape(ty: &TSType) -> Result<TypeShape, String> {
    Ok(match ty {
        TSType::TSBooleanKeyword(_) => TypeShape::Bool,
        // TS `number` lowers to F64 by default (see dag::TypeShape docs).
        TSType::TSNumberKeyword(_) => TypeShape::F64,
        TSType::TSBigIntKeyword(_) => TypeShape::S64,
        // THESE TWO ARMS ARE THE WHOLE TS->TypeShape NUMERIC SURFACE. `bigint`
        // above and `number` on the line before it are the only ways a numeric
        // TypeShape is reached from source, so `S32`, `F32`, `U32` and `U64`
        // have NO authored spelling and arrive only from a declared `FuncSig`
        // or another non-TS producer. TypeScript has neither unsigned types nor
        // width annotations, so a spelling needs a convention the language does
        // not supply - a branded alias, a declared alias this parser
        // recognises, or a decorator - and that choice is DEFERRED rather than
        // guessed at (`dag::TypeShape`). This is where it lands when it is
        // made; nothing above should quietly grow a fifth reading in the
        // meantime.
        TSType::TSStringKeyword(_) => TypeShape::String,
        TSType::TSArrayType(arr) => TypeShape::List(Box::new(type_shape(&arr.element_type)?)),
        TSType::TSParenthesizedType(p) => type_shape(&p.type_annotation)?,
        TSType::TSTypeLiteral(lit) => TypeShape::Record(signatures_to_fields(&lit.members)?),
        TSType::TSUnionType(u) => union_shape(u)?,
        TSType::TSTypeReference(r) => reference_shape(r)?,
        TSType::TSIndexedAccessType(idx) => indexed_shape(idx)?,
        // A LITERAL IN TYPE POSITION, which used to fall to the `_` arm below
        // and become `Named("unknown")` - see [`literal_shape`].
        TSType::TSLiteralType(lit) => literal_shape(lit)?,
        // Anything else we don't model becomes an opaque named reference.
        _ => TypeShape::Named("unknown".to_string()),
    })
}

/// `Outer["field"]` - TypeScript's indexed access, as the NODE it is.
///
/// # It used to be a `Named` holding the brackets, and that was the defect
///
/// MEASURED, before the indexed access had a node of its own:
///
/// ```text
/// Person["handle"]  ->  Named("Person[\"handle\"]")
/// ```
///
/// and `libhbdata::typeexpr::eval` carried that string on into the FINAL
/// vocabulary as `ShapeType::Named("Person[\"handle\"]")` - a name no
/// declaration answers, standing where the field's own type belonged. The
/// relationship was TEXT INSIDE A NAME, which is what RULING 4 forbids, and it
/// survived the strict lowering with no gate to say so. The arm was admitted
/// because the spelling was already the carrier one layer up
/// (`highbay_ui::zui::forms` builds `"HomeProps[\"user\"]"` in Rust and
/// `libhbui::app`'s `fields_of` takes the brackets apart), so an authored
/// document and a Rust-built tree agreed - on the wrong node.
///
/// **What the node changes is where the link LIVES**, not whether the spelling
/// is accepted: the base is the same `Named("Person")` every other reference to
/// Person is, the key is a key, and evaluation reduces it to the FIELD'S OWN
/// TYPE rather than to a dangling name. It is a registered type function now
/// rather than a variant - [`TypeShape::indexed_access`] builds it and
/// [`TypeShape::INDEXED_ACCESS`] names it - which changes the CARRIER and none
/// of the above.
///
/// # The base takes the ordinary lowering
///
/// Any type expression, not just a name - which is what makes
/// `Person["address"]["city"]` this node nested rather than a refusal. The
/// previous arm required a `Named` base and only accepted that nesting by
/// accident, because the inner access had already collapsed INTO a name.
///
/// # The key must be a string literal HERE, and `Pick`'s no longer is
///
/// The two look like one rule and are not. `Pick<T, K>` lowers `K` through
/// `type_shape` and reaches the registry faithfully as `Named("K")`, so the
/// refusal belongs at evaluation. `Home[keyof X]` has no such luck: `keyof X`
/// falls to [`type_shape`]'s `_ =>` arm and becomes `Named("unknown")`, which
/// is the degradation this whole area exists to stop - so the refusal stays
/// here, where the source is still in hand to name.
///
/// It is a DIFFERENT refusal from the one that left `Pick`: that one was
/// `key_names` refusing a faithful lowering; this one is refusing to write down
/// a mangled one.
fn indexed_shape(idx: &oxc_ast::ast::TSIndexedAccessType) -> Result<TypeShape, String> {
    let TSType::TSLiteralType(lit) = &idx.index_type else {
        return Err("an indexed access `T[K]` takes a string literal key".to_string());
    };
    let oxc_ast::ast::TSLiteral::StringLiteral(key) = &lit.literal else {
        return Err("an indexed access `T[K]` takes a string literal key".to_string());
    };
    Ok(TypeShape::indexed_access(type_shape(&idx.object_type)?, key.value.to_string()))
}

/// **A union.** The nullish members are partitioned out into
/// [`TypeShape::Option`]; what is left is the union proper.
///
/// # It used to collapse, then it REFUSED, and this is the third answer
///
/// The first rule was "other unions collapse to the first non-nullish member
/// (best-effort)", so `Id | Blank` parsed as `Id` and `Blank` disappeared with
/// no diagnostic anywhere. That is worse than unsupported: the author declared
/// a sum type, the parse answered with one arm of it, and every reader
/// downstream - the property sheet, the daemon's column planner, the seed
/// generator - saw a complete declaration that was not the one written.
///
/// So it became a refusal, correctly: [`TypeShape`] modelled products
/// (`Record`) and options and had no sum, and refusing says so at the one place
/// that knows. But the refusal was never the destination - its own doc named
/// the fix (*"admitting it needs a `TypeShape` variant"*), and the price of
/// standing still was `tone?: "primary" | "danger"`, ordinary TSX, failing to
/// parse at all.
///
/// [`TypeShape::Union`] is that variant, so the whole of this function's
/// refusal is now a construction. **The anti-collapse property is unchanged and
/// is what the test still measures**: every member reaches the shape. Nothing
/// is dropped, which was the only thing the refusal was protecting.
///
/// # The nullish members still come out, and `Option` is still outermost
///
/// `A | B | undefined` lowers to `Option(Union([A, B]))`, not to
/// `Union([A, B, Undefined])`: there is no `undefined` [`TypeShape`], and
/// minting one so that `Option` could be expressed as a union would replace a
/// modelled absence with a member every consumer has to recognise by name. See
/// [`TypeShape::Union`] for the full argument.
fn union_shape(u: &oxc_ast::ast::TSUnionType) -> Result<TypeShape, String> {
    let mut nullish = false;
    let mut members: Vec<&TSType> = Vec::new();
    for t in &u.types {
        match t {
            TSType::TSUndefinedKeyword(_) | TSType::TSNullKeyword(_) => nullish = true,
            other => members.push(other),
        }
    }
    // A union of ONE is that one type - a `Union` wrapper around a single
    // member would make `A` and `(A)` two different shapes, and the author
    // wrote one type either way. `TypeShape::Union` documents this
    // normalization as the producer's job rather than the type's.
    let shape = match members.as_slice() {
        // `undefined | null` alone: nullish and nothing to be optional ABOUT.
        [] => return Ok(TypeShape::Named("unknown".to_string())),
        [only] => type_shape(only)?,
        many => TypeShape::Union(
            many.iter().map(|m| type_shape(m)).collect::<Result<Vec<_>, _>>()?,
        ),
    };
    Ok(if nullish { TypeShape::Option(Box::new(shape)) } else { shape })
}

/// **A literal in TYPE position** - `"handle"`, `42`, `-1`, `true`.
///
/// # It was `Named("unknown")`, and that is the defect this replaces
///
/// A `TSLiteralType` had no arm at all, so it fell through [`type_shape`]'s
/// `_ =>` catch-all. MEASURED, before this existed:
///
/// ```text
/// interface Row { kind: "handle" }  ->  kind: Named("unknown")
/// ```
///
/// which is the same shape an unmodelled `symbol`, `never` or mapped type
/// produces. A discriminant a record keys on and a form this vocabulary has
/// never heard of decoded IDENTICALLY, and the catch-all is why: it answers
/// "this is a reference to something I cannot see", which is true of an
/// unmodelled type and false of a literal, whose whole content is right there
/// in the source.
///
/// # The numeric capture is [`numeric_literal`]'s, not a second reading
///
/// Reusing it is the point: it reads the INTEGER FROM THE SOURCE TEXT rather
/// than from oxc's already-rounded `f64`, which is the only reading under which
/// an `Int64` capture is worth having. A literal type written past 2^53 has
/// exactly the same fidelity as the same token written in value position,
/// because it is the same function.
///
/// # Two refusals, both named
///
/// * a **bigint** literal type (`1n`) - [`LiteralValue`] has no bigint carrier,
///   and minting one at the type level alone would create a literal the value
///   level cannot hold;
/// * a **template literal** type with no substitution (`` `abc` ``) - which is
///   a literal string wearing the other quotes, and would be admissible; it is
///   refused because admitting it would make `` `abc` `` and `"abc"` the same
///   shape and the emit would have to pick one spelling to write back.
///
/// Both are refused rather than degraded, because degrading is what this
/// function exists to stop. The refusal names the form so an author can find
/// it.
///
/// **A template literal type WITH a substitution (`` `id-${string}` ``) does
/// not arrive here at all** - oxc gives it a `TSTemplateLiteralType`, a
/// different node, which still falls to [`type_shape`]'s `_ =>` arm and becomes
/// `Named("unknown")`. MEASURED. That is the pre-existing catch-all rather than
/// anything this function decides, and it is a real gap of the same class: a
/// type-level function over strings is §13's registry territory, not a literal.
fn literal_shape(lit: &oxc_ast::ast::TSLiteralType) -> Result<TypeShape, String> {
    use oxc_ast::ast::TSLiteral;
    let value = match &lit.literal {
        TSLiteral::StringLiteral(s) => LiteralValue::String(s.value.to_string()),
        TSLiteral::BooleanLiteral(b) => LiteralValue::Bool(b.value),
        TSLiteral::NumericLiteral(n) => numeric_literal(n),
        // A NEGATIVE numeric literal type, which TypeScript spells as a unary
        // expression rather than as a token: `type Step = -1 | 0 | 1`. Accepting
        // `1` and refusing `-1` would be a half of a form authors write whole.
        TSLiteral::UnaryExpression(u) => return unary_literal_shape(u),
        TSLiteral::BigIntLiteral(_) => {
            return Err("a bigint literal type has no place in this vocabulary - \
                        there is no bigint literal to hold it"
                .to_string());
        }
        TSLiteral::TemplateLiteral(_) => {
            return Err("a template literal type is not modelled - it is a type-level \
                        function over strings, not a literal"
                .to_string());
        }
    };
    Ok(TypeShape::Literal(value))
}

/// `-1` / `+1` in type position: the negation applied to the literal it wraps.
///
/// Split out of [`literal_shape`] so the two refusals it adds - a non-numeric
/// operand and an operator that is not a sign - read as refusals rather than as
/// nesting. Both are unreachable from TypeScript that type-checks; they are
/// here because the AST can hold them and answering "unknown" is what this pair
/// of functions exists to stop doing.
fn unary_literal_shape(u: &oxc_ast::ast::UnaryExpression) -> Result<TypeShape, String> {
    let Expression::NumericLiteral(n) = &u.argument else {
        return Err("a literal type's operand is a number".to_string());
    };
    let value = numeric_literal(n);
    let value = match u.operator {
        UnaryOperator::UnaryPlus => value,
        UnaryOperator::UnaryNegation => match value {
            // `numeric_literal` produces only these two, and the one integer
            // that cannot be negated (`i64::MIN`) is not one of them: its
            // magnitude does not parse as an `i64`, so the token arrives as a
            // float already.
            LiteralValue::Int64(v) => match v.checked_neg() {
                Some(v) => LiteralValue::Int64(v),
                None => return Err("a negated literal type is out of range".to_string()),
            },
            LiteralValue::Float64(v) => LiteralValue::Float64(-v),
            other => return Err(format!("`-` does not apply to the literal {other:?}")),
        },
        other => {
            return Err(format!(
                "`{}` is not a sign, and a literal type takes only a sign",
                other.as_str()
            ));
        }
    };
    Ok(TypeShape::Literal(value))
}

/// `Array<T>` → `List<T>`; every other generic reference preserves its
/// constructor and all arguments as [`TypeShape::Apply`].
///
/// # `Omit`/`Pick` had a branch here, and it is GONE
///
/// It read the key argument SYNTACTICALLY - `key_names`, string literals only -
/// because `TypeShape::Omit` held its keys as a `Vec<String>` and `Apply`
/// demanded a type in that slot. The branch existed because the general path
/// could only MANGLE the argument: `"a"` fell to `type_shape`'s `_ =>` arm and
/// became `Named("unknown")`, and `"a" | "b"` reached [`union_shape`], which
/// refused a two-member union outright.
///
/// Both of those are faithful now - the first is a `Literal`, the second a
/// `Union` - so the general path destroys nothing, and the operators are
/// registered type functions rather than variants (FACT_IMPLEMENTATION A3).
/// **The key slot therefore holds THE UNION THE TS SPELLING ALWAYS HAD**, which
/// is what FACT_CURRENT_v2 section 12 asked for.
///
/// **What this newly ADMITS is `Pick<T, K>` with `K` a type parameter**, which
/// `key_names` refused outright. It parses as `Apply { "Pick", [Named("T"),
/// Named("K")] }` - faithful to the source - and is refused at EVALUATION, with
/// the base in hand, which is where every other key question is already
/// answered.
fn reference_shape(r: &oxc_ast::ast::TSTypeReference) -> Result<TypeShape, String> {
    let name = match &r.type_name {
        oxc_ast::ast::TSTypeName::IdentifierReference(id) => id.name.to_string(),
        _ => return Ok(TypeShape::Named("unknown".to_string())),
    };
    let Some(type_arguments) = &r.type_arguments else {
        return Ok(TypeShape::Named(name));
    };
    key_operator_or_apply(&name, &type_arguments.params)
}

/// **`Name<args…>`, however it was written.** Shared by the field position
/// ([`reference_shape`]) and the `extends` clause ([`heritage_shape`]), because
/// `Omit<B, "a">` must mean the same thing in both and oxc hands them over as
/// two different node types.
///
/// `Array` is the ONE name still special-cased here, and it is not an operator:
/// `Array<T>` and `T[]` are two spellings of one TypeScript type, so they lower
/// to the one node. Every other constructor - including `Omit`, `Pick` and
/// `Partial` - is carried by name to the registry.
fn key_operator_or_apply(
    name: &str,
    params: &oxc_allocator::Vec<'_, TSType<'_>>,
) -> Result<TypeShape, String> {
    let args = params.iter().map(type_shape).collect::<Result<Vec<_>, _>>()?;
    if name == "Array" {
        if let Some(first) = args.first() {
            return Ok(TypeShape::List(Box::new(first.clone())));
        }
    }
    Ok(TypeShape::Apply { constructor: name.to_string(), args })
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
        // announces an event binding, so the value is lowered as an imported
        // call right here. There is no later pass that reinterprets an
        // `AttrValue::Opaque`, which is exactly why an `on..` attribute can
        // never quietly become one.
        let value = if is_event_binding(&key) {
            imported_call_attr(&key, a.value.as_ref(), &low.scope)?
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

/// **JSX text whitespace, as JSX defines it.**
///
/// This was `t.value.trim()` until 2026-08-29, which destroyed the space
/// beside an element child at the parse: `<Content>When <Text/> in the
/// table.</Content>` reached the tree as `Text("When")`, the element,
/// `Text("in the table.")` and drew welded as `Whenin`. The loss was at the
/// parse, so nothing downstream could recover it, and inline placement
/// (`designs/NEW_CONTENT_LAYOUT.md` step 7) could not be spelled with a space.
///
/// The rule implemented here is the standard one - Babel's
/// `cleanJSXElementLiteralChild`, which is what every JSX author already
/// expects - and NOT a variant:
///
/// 1. split the text child on line breaks (`\r\n`, `\n`, `\r`);
/// 2. tabs become spaces;
/// 3. strip leading spaces on every line EXCEPT the first;
/// 4. strip trailing spaces on every line EXCEPT the last;
/// 5. drop lines that are then empty;
/// 6. join the survivors with one space between them - every surviving line
///    except the LAST NON-EMPTY one gains a single trailing space;
/// 7. if nothing survives, emit no text node at all (`None`).
///
/// Rules 3 and 4 are what make this agree with `trim()` for pretty-printed
/// TSX, which is the whole blast radius argument: a child on its own indented
/// line arrives as `"\n      A paragraph.\n    "`, whose first and last lines
/// are empty and dropped and whose middle line is both stripped of its indent
/// and the last non-empty one, so it gains no trailing space. The two rules
/// differ in exactly three places, and the corpus has none of them: the inline
/// case above; a single-line child with deliberate inner padding, where
/// `<Text> Hello </Text>` is `" Hello "` here and was `"Hello"` under `trim()`
/// because one line is both the first and the last so neither strip applies;
/// and a child whose PROSE spans lines, where `trim()` kept the newline and
/// the next line's indent inside the string and this joins them with one
/// space. MEASURED 2026-08-29 over all 66 authored `.tsx` under `crates/` and
/// `data/` plus this crate's own fixtures: ZERO text children change.
fn clean_jsx_text(raw: &str) -> Option<String> {
    let lines = split_jsx_lines(raw);
    // "Empty" for the join is judged on the RAW line - spaces and tabs only -
    // which is the same question rule 5 asks after the strips, except for the
    // one line that is both first and last and so keeps its padding.
    let last_non_empty = lines
        .iter()
        .rposition(|line| line.bytes().any(|b| b != b' ' && b != b'\t'))
        .unwrap_or(0);

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let mut piece = line.replace('\t', " ");
        if i != 0 {
            // whitespace touching a newline on its left
            piece = piece.trim_start_matches(' ').to_string();
        }
        if i + 1 != lines.len() {
            // whitespace touching a newline on its right
            piece = piece.trim_end_matches(' ').to_string();
        }
        if piece.is_empty() {
            continue;
        }
        out.push_str(&piece);
        if i != last_non_empty {
            out.push(' ');
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

/// The line split rule 1 asks for: `\r\n`, `\n` and `\r` each end a line, and
/// a trailing break yields a final empty line (which is what makes the last
/// line of `"text\n    "` droppable rather than the text's own trailing run).
fn split_jsx_lines(raw: &str) -> Vec<&str> {
    let bytes = raw.as_bytes();
    let mut lines = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                lines.push(&raw[start..i]);
                i += if bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                start = i;
            }
            b'\n' => {
                lines.push(&raw[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&raw[start..]);
    lines
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
            if let Some(txt) = clean_jsx_text(t.value.as_str()) {
                out.push(Node::Text(txt));
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
/// use libtsx::dag::{BindingExpr, LiteralValue};
///
/// let expr = BindingExpr::try_from(r#"props.label ?? "none""#).expect("lowers");
/// assert_eq!(
///     expr,
///     BindingExpr::Coalesce(vec![
///         BindingExpr::Path(vec!["props".into(), "label".into()]),
///         BindingExpr::Literal(LiteralValue::String("none".into())),
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
/// [`ImportScope`] to hold. Nothing on this path consults one either: a scope
/// is read only by the `on..` EVENT grammar ([`imported_call_attr`]), which resolves a
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
            &ImportScope {
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
fn lower_binding_expr(expr: &Expression, _scope: &ImportScope) -> Result<BindingExpr, String> {
    use BindingExpr as B;
    let expr = unparen(expr);
    match expr {
        Expression::NullLiteral(_) => Ok(B::Null),
        Expression::BooleanLiteral(v) => Ok(B::Literal(LiteralValue::Bool(v.value))),
        Expression::NumericLiteral(v) => Ok(B::Literal(numeric_literal(v))),
        Expression::StringLiteral(v) => Ok(B::Literal(LiteralValue::String(v.value.to_string()))),
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
            // ONE HOP PER NODE, nesting outward. `f().x.y` is
            // `MemberOf(MemberOf(<f()>, x), y)` - the segments were peeled
            // inner-first above and reversed, so folding them in source order
            // rebuilds the chain in the order they were written. A `Vec` of
            // segments hanging off one base is the shape `Member` had and the
            // reason it went: it gave a hop off a hop a spelling that is not an
            // expression.
            Ok(segments.into_iter().fold(base, |base, segment| B::MemberOf(
                Box::new(base),
                PropertyAccessor::intern(&segment),
            )))
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
            let type_args: Vec<TypeShape> = call
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
            // **A BARE identifier call is a SYMBOL, not a `Call`.** `it()`,
            // `design()`, `uiSite()` - no namespace, no type argument, no
            // argument - are the form
            // [`SymbolValue`](crate::dag::BindingExpr::SymbolValue) captures,
            // and the strictness is what keeps one authored spelling to one
            // shape: were both spellings producible, `foo()` would have two
            // representations and every consumer would owe both an arm.
            //
            // This decides nothing about what an identifier MEANS. An
            // identifier with no interned variant becomes
            // [`ObjectSymbol::Named`], because refusing an unknown callee is a
            // judgement only a consumer holding a scope can make.
            if namespace.is_empty() && type_args.is_empty() && args.is_empty() {
                return Ok(B::SymbolValue(ObjectSymbol::intern(&name)));
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
        // `===` and `==`, and ONLY those two of oxc's binary operators. Every
        // ordering comparison is refused for want of anything asking for one.
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
        // `!==` and `!=`, lowered to their OWN variant and never to a negated
        // equality. `a != b` and `!(a == b)` are two things an author can
        // write, and a capture that produced one shape for both would leave
        // the emitter guessing which - see [`BindingExpr::Ne`]. Nothing is
        // inferred in either direction here: this arm never builds a `Not`,
        // and the unary arm below never builds a `Ne`.
        Expression::BinaryExpression(binary)
            if matches!(
                binary.operator,
                BinaryOperator::StrictInequality | BinaryOperator::Inequality
            ) =>
        {
            Ok(B::Ne {
                left: Box::new(lower_binding_expr(&binary.left, _scope)?),
                right: Box::new(lower_binding_expr(&binary.right, _scope)?),
                strict: binary.operator == BinaryOperator::StrictInequality,
            })
        }
        // The prefix `!`, and ONLY it of oxc's unary operators - `-`, `+`,
        // `~`, `typeof`, `void` and `delete` share this node kind and stay
        // refused, each being a different operator nothing has asked for. The
        // arm is written on the OPERATOR rather than the node for exactly the
        // reason the `??` arm above is: matching the node would silently lower
        // `typeof x` as a negation.
        Expression::UnaryExpression(unary)
            if unary.operator == UnaryOperator::LogicalNot =>
        {
            Ok(B::Not(Box::new(lower_binding_expr(&unary.argument, _scope)?)))
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
    scope: &ImportScope,
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
    scope: &ImportScope,
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
    scope: &ImportScope,
) -> Result<Vec<BlockStmt>, String> {
    let mut out = Vec::new();
    for statement in statements {
        out.extend(lower_block_statement(statement, scope)?);
    }
    Ok(out)
}

fn lower_block_statement(
    statement: &Statement,
    scope: &ImportScope,
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
        // Reached for every binary operator EXCEPT the four equalities
        // (`===`/`==`/`!==`/`!=`), which have positive arms. The inequalities
        // used to be named individually HERE, because "we support equality"
        // and "we refused your `!==`" are one keystroke apart; they lower now,
        // so the naming moved from the refusal to the capture and the arms
        // that spelled it are gone rather than left as dead reassurance.
        Expression::BinaryExpression(binary) => match binary.operator {
            BinaryOperator::LessThan
            | BinaryOperator::LessEqualThan
            | BinaryOperator::GreaterThan
            | BinaryOperator::GreaterEqualThan => "ordering-comparison",
            _ => "binary-operator",
        },
        // Reached for every unary operator EXCEPT `!`, which has a positive
        // arm. Named individually for the reason the inequalities once were:
        // an author who writes `typeof x` and is told only "unary-operator"
        // has no way to tell whether `!x` went the same way.
        Expression::UnaryExpression(unary) => match unary.operator {
            UnaryOperator::UnaryNegation => "`-`",
            UnaryOperator::UnaryPlus => "`+`",
            UnaryOperator::BitwiseNot => "`~`",
            UnaryOperator::Typeof => "`typeof`",
            UnaryOperator::Void => "`void`",
            UnaryOperator::Delete => "`delete`",
            // `!` has a positive arm and cannot reach a refusal from here.
            // The arm exists because the match is exhaustive; it names the
            // operator anyway, so a future path that did reach it would say
            // something true rather than something reassuring.
            UnaryOperator::LogicalNot => "`!`",
        },
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
            AttrValue::BindingExpr(BindingExpr::Literal(LiteralValue::Int64(3)))
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

    /// **The key operators keep the KEYS** - the property that earned them
    /// dedicated variants, held at a different address now that they are
    /// registered type functions (FACT_IMPLEMENTATION A3).
    ///
    /// Before the variants existed, `Omit<ContainerProps, "direction">`
    /// measured as `Apply { "Omit", [Named("ContainerProps"),
    /// Named("unknown")] }` - the field name replaced by the parser's word for
    /// "not modelled", so two Omits hiding DIFFERENT fields of one base were
    /// byte-identical. The `assert_ne` below is the specific thing that used to
    /// be an `assert_eq`, and it is asserted over the APPLICATION now: the key
    /// slot holds the union it always had in the TS spelling, so the general
    /// node carries the names the variant used to.
    #[test]
    fn the_key_operators_keep_their_field_names() {
        let interfaces = extract_interfaces(
            r#"
                interface Props {
                    row: Omit<ContainerProps, "direction">;
                    pair: Omit<ContainerProps, "direction" | "gap">;
                    just: Pick<ContainerProps, "gap">;
                    other: Omit<ContainerProps, "gap">;
                }
            "#,
        )
        .expect("the key operators parse");
        let base = || TypeShape::Named("ContainerProps".into());

        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::key_operator("Omit", base(), vec!["direction".into()]),
        );
        assert_eq!(
            interfaces[0].fields[1].ty,
            TypeShape::key_operator("Omit", base(), vec!["direction".into(), "gap".into()]),
            "a union of literals is several keys, not a refused union",
        );
        assert_eq!(
            interfaces[0].fields[2].ty,
            TypeShape::key_operator("Pick", base(), vec!["gap".into()]),
        );
        assert_ne!(
            interfaces[0].fields[0].ty, interfaces[0].fields[3].ty,
            "two Omits over one base hiding different fields must not be equal",
        );
        assert_ne!(
            interfaces[0].fields[2].ty, interfaces[0].fields[3].ty,
            "Pick and Omit over one base with one key are duals, not the same",
        );
    }

    /// **THE KEY SLOT IS THE UNION IT ALWAYS WAS**, spelled out rather than
    /// gone through the constructor - because the constructor and the parser
    /// agreeing proves only that they agree.
    ///
    /// `Pick<P, "a" | "b">`'s second type argument is `"a" | "b"`, which is a
    /// union of two string literals in TypeScript and is that exactly here. The
    /// single-key case is the literal itself and NOT a one-member union, under
    /// `TypeShape::Union`'s producer-normalization rule.
    #[test]
    fn a_key_argument_is_a_literal_or_a_union_of_them() {
        let interfaces = extract_interfaces(
            r#"
                interface Props {
                    one: Pick<P, "a">;
                    two: Pick<P, "a" | "b">;
                }
            "#,
        )
        .expect("the key operators parse");
        let TypeShape::Apply { constructor, args } = &interfaces[0].fields[0].ty else {
            panic!("an application, not a variant: {:?}", interfaces[0].fields[0].ty)
        };
        assert_eq!(constructor, "Pick");
        assert_eq!(args[1], TypeShape::Literal(LiteralValue::String("a".into())));

        let TypeShape::Apply { args, .. } = &interfaces[0].fields[1].ty else {
            panic!("an application")
        };
        assert_eq!(
            args[1],
            TypeShape::Union(vec![
                TypeShape::Literal(LiteralValue::String("a".into())),
                TypeShape::Literal(LiteralValue::String("b".into())),
            ]),
        );
    }

    /// **`Pick<T, K>` WITH A TYPE PARAMETER PARSES**, which `key_names`
    /// refused outright - FACT_IMPLEMENTATION A3's named red.
    ///
    /// It is not resolvable here and is not meant to be: `K` lowers to the name
    /// it is, faithfully, and the refusal moves to evaluation where the base is
    /// in hand. The old refusal happened with only a fragment of the document
    /// and could therefore only ever say "not every type is a field name".
    #[test]
    fn a_key_argument_may_be_a_type_parameter() {
        let interfaces = extract_interfaces(r#"interface Props { sub: Pick<T, K>; }"#)
            .expect("a generic key argument parses");
        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::Apply {
                constructor: "Pick".into(),
                args: vec![TypeShape::Named("T".into()), TypeShape::Named("K".into())],
            },
        );
    }

    /// They NEST, which is why the base is an ordinary type argument.
    #[test]
    fn the_key_operators_compose() {
        let interfaces = extract_interfaces(
            r#"interface Props { narrowed: Pick<Omit<Full, "a">, "b">; }"#,
        )
        .expect("nested operators parse");
        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::key_operator(
                "Pick",
                TypeShape::key_operator("Omit", TypeShape::Named("Full".into()), vec!["a".into()]),
                vec!["b".into()],
            ),
        );
    }

    /// **The `extends` clause reaches the IR** - it used to be dropped with no
    /// error at all, which is the defect `TypeShape::Extends` exists for.
    ///
    /// The `assert!(!.extends.is_empty())` is the specific thing that was false:
    /// `interface P extends B {}` parsed to an interface with no fields AND no
    /// record of the clause, so nothing downstream could tell it apart from
    /// `interface P {}`.
    #[test]
    fn an_extends_clause_is_not_dropped() {
        let interfaces = extract_interfaces(
            r#"
                interface HBoxProps extends Omit<ContainerProps, "direction"> {}
                interface Two extends A, B { own: string }
            "#,
        )
        .expect("the clause parses");

        assert!(
            !interfaces[0].extends.is_empty(),
            "the clause was dropped - the exact silent failure this closed",
        );
        assert_eq!(
            interfaces[0].extends,
            vec![TypeShape::Extends {
                base: Box::new(TypeShape::key_operator(
                    "Omit",
                    TypeShape::Named("ContainerProps".into()),
                    vec!["direction".into()],
                )),
            }],
            "an operator in the clause is the SAME tree it is in field position",
        );

        assert_eq!(
            interfaces[1].extends,
            vec![
                TypeShape::Extends { base: Box::new(TypeShape::Named("A".into())) },
                TypeShape::Extends { base: Box::new(TypeShape::Named("B".into())) },
            ],
            "several entries, in source order",
        );
        assert_eq!(
            interfaces[1].fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["own"],
            "the body is still read alongside the clause",
        );
    }

    /// An interface with NO clause records none - absent is not a default.
    #[test]
    fn a_plain_interface_extends_nothing() {
        let interfaces =
            extract_interfaces("interface P { a: string }").expect("it parses");
        assert!(interfaces[0].extends.is_empty());
    }

    /// **`Partial` is deliberately NOT one of them.** Its argument is a type,
    /// so `Apply` carries it faithfully and there is nothing to repair; it
    /// reduces at resolution by applying Optional to each field of the resolved
    /// base, which `TypeShape::Option` already spells.
    ///
    /// Asserted so that "add Partial too, for symmetry" fails a test that says
    /// why not to, rather than looking like an oversight.
    #[test]
    fn partial_stays_an_ordinary_application() {
        let interfaces =
            extract_interfaces(r#"interface Props { draft: Partial<User>; }"#).expect("parses");
        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::Apply {
                constructor: "Partial".into(),
                args: vec![TypeShape::Named("User".into())],
            },
        );
    }

    /// **An indexed access keeps its BASE AND ITS KEY APART**, which is what a
    /// name holding brackets never did.
    ///
    /// Two earlier states, both measured, and the second is why the variant
    /// exists:
    ///
    /// 1. Before `indexed_shape` existed at all this fell to `type_shape`'s
    ///    `_ =>` arm and became `Named("unknown")` - the base GONE - so a `Pick`
    ///    over a nested shape resolved against nothing and drew an EMPTY form
    ///    step. The first `assert_ne` is that state.
    /// 2. Then it was `Named("Home[\"user\"]")`: the base was back, but as TEXT
    ///    INSIDE A NAME, which is the shape RULING 4 forbids and which
    ///    `libhbdata::typeexpr` carried on into the final vocabulary as a name
    ///    nothing answers. The second `assert_ne` is that one.
    ///
    /// The `Pick` beside it is not decoration: the base is boxed so the
    /// operators compose, and this is the composition the corpus already writes
    /// (`crates/highbay_elements/data/examples/forms_screen.tsx`).
    #[test]
    fn an_indexed_access_keeps_the_member_path_it_names() {
        let interfaces = extract_interfaces(
            r#"
                interface Props {
                    user: Home["user"];
                    picked: Pick<Home["user"], "name" | "email">;
                }
            "#,
        )
        .expect("an indexed access parses");
        let user = TypeShape::indexed_access(TypeShape::Named("Home".into()), "user");
        assert_eq!(interfaces[0].fields[0].ty, user);
        assert_eq!(
            interfaces[0].fields[1].ty,
            TypeShape::key_operator(
                "Pick",
                user.clone(),
                vec!["name".into(), "email".into()],
            ),
        );
        assert_ne!(interfaces[0].fields[0].ty, TypeShape::Named("unknown".into()));
        assert_ne!(interfaces[0].fields[0].ty, TypeShape::Named("Home[\"user\"]".into()));
    }

    /// **The spelling survives the round trip**, which is the other half of a
    /// vocabulary: a type that parses and cannot be written back is not in the
    /// language, it is only tolerated by the parser.
    ///
    /// Nested both ways, because that is where a shared spelling helper earns
    /// its keep - the brackets are written in exactly one place
    /// (`dag::indexed_access_spelling`, reached through
    /// `dag::application_spelling`) and the key quotes in another
    /// (`dag::literal_spelling`, reached through the union), so neither can come
    /// out one way here and another way in a drawn label.
    #[test]
    fn an_indexed_access_is_written_back_as_the_typescript_it_was_read_from() {
        let interfaces = extract_interfaces(
            r#"
                interface Props {
                    user: Home["user"];
                    deep: Home["user"]["name"];
                    picked: Pick<Home["user"], "name" | "email">;
                }
            "#,
        )
        .expect("parses");
        let spelled: Vec<String> =
            interfaces[0].fields.iter().map(|f| String::from(&f.ty)).collect();
        assert_eq!(
            spelled,
            vec![
                "Home[\"user\"]".to_string(),
                "Home[\"user\"][\"name\"]".to_string(),
                "Pick<Home[\"user\"], \"name\" | \"email\">".to_string(),
            ],
        );
    }

    /// A key that is not a string literal is REFUSED rather than spelled
    /// best-effort: `Home[keyof X]` names something this vocabulary cannot
    /// write down, and inventing a spelling puts `unknown` back by another
    /// door.
    #[test]
    fn an_indexed_access_with_a_computed_key_is_refused() {
        let err = extract_interfaces(r#"interface Props { bad: Home[keyof Home]; }"#)
            .expect_err("a computed key is refused")
            .join("; ");
        assert!(err.contains("string literal key"), "got: {err}");
    }

    /// **THE TWO KEY REFUSALS MOVED TO EVALUATION, AND THIS IS WHAT REPLACED
    /// THEM HERE: the parse is FAITHFUL.**
    ///
    /// `Omit<Base, number>` and `Omit<Base>` were refused at parse, by
    /// `key_names` and by an arity check this function no longer performs.
    /// Both now lower to the application they are, because `Omit` is a
    /// registered type function and this crate holds no registry - it has no
    /// evaluator and nowhere to host a reduction, which is FACT_CURRENT_v2
    /// section 13 rule 2 read correctly (what `libtsx` owned was the PARSE-side
    /// hardcoding, and that is the thing that left).
    ///
    /// **What this test must therefore still prove is that nothing DEGRADES**,
    /// which was always the real content of the refusal: a key slot holding
    /// `Named("unknown")` is the defect, and a key slot holding the type the
    /// author wrote is not. `number` is `F64` here, faithfully, and the
    /// refusal is `libhbdata::typeexpr`'s - `a_key_argument_that_is_not_a_field_name_is_refused`
    /// and `an_arity_mismatch_is_its_own_refusal`, where the base is in hand
    /// and the diagnostic can name what the keys should have been.
    #[test]
    fn a_key_that_is_not_a_name_lowers_faithfully_and_is_refused_at_evaluation() {
        let interfaces = extract_interfaces(r#"interface Props { bad: Omit<Base, number>; }"#)
            .expect("a non-literal key parses as the type it is");
        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::Apply {
                constructor: "Omit".into(),
                args: vec![TypeShape::Named("Base".into()), TypeShape::F64],
            },
            "the key slot holds what was written, never `unknown`",
        );
        assert_eq!(
            interfaces[0].fields[0].ty.key_literals(),
            None,
            "and it does not READ as a key list, which is what the evaluator refuses on",
        );

        let interfaces = extract_interfaces(r#"interface Props { bad: Omit<Base>; }"#)
            .expect("a one-argument Omit parses");
        assert_eq!(
            interfaces[0].fields[0].ty,
            TypeShape::Apply {
                constructor: "Omit".into(),
                args: vec![TypeShape::Named("Base".into())],
            },
            "the arity is preserved for the registry to refuse, not corrected here",
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

    /// **A union of two real types is CARRIED, not collapsed** - the defect
    /// [`union_shape`] documents, now fixed rather than refused. `Id | Blank`
    /// once parsed as `Id`, so a declared sum type reached every reader as one
    /// arm of itself with no diagnostic anywhere; then it was refused outright,
    /// because the vocabulary had no sum to lower it to.
    ///
    /// **The property under test did not change when the answer did.** What was
    /// always being ruled out is a member DISAPPEARING, and that is asserted
    /// here the direct way: both members are in the shape. The refusal was one
    /// way to guarantee it and [`TypeShape::Union`] is a better one, because it
    /// also parses.
    #[test]
    fn a_union_of_two_real_types_is_carried_rather_than_collapsed() {
        let ifaces = extract_interfaces("interface Route { record: Id | Blank; }")
            .expect("a sum type is spellable now");
        assert_eq!(
            ifaces[0].fields[0].ty,
            TypeShape::Union(vec![
                TypeShape::Named("Id".into()),
                TypeShape::Named("Blank".into()),
            ]),
            "neither member is dropped, and they keep the author\'s order",
        );

        // ...and the nullish forms are untouched: they are an Option, which this
        // vocabulary has always modelled and which the union does NOT subsume.
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

        // The two compose, with `Option` OUTERMOST - `A | B | undefined` is an
        // optional union and not a union with an `undefined` member.
        let ifaces =
            extract_interfaces("interface P { a: Id | Blank | undefined; }").expect("optional union");
        assert_eq!(
            ifaces[0].fields[0].ty,
            TypeShape::Option(Box::new(TypeShape::Union(vec![
                TypeShape::Named("Id".into()),
                TypeShape::Named("Blank".into()),
            ]))),
        );

        // A nested union reaches the shape through the containers too - a record
        // field and a list element are the two ways one used to hide.
        let union = TypeShape::Union(vec![
            TypeShape::Named("Id".into()),
            TypeShape::Named("Blank".into()),
        ]);
        let ifaces = extract_interfaces("interface P { a: { b: Id | Blank }; }").expect("record");
        let TypeShape::Record(fields) = &ifaces[0].fields[0].ty else { panic!("a record") };
        assert_eq!(fields[0].ty, union);
        let ifaces = extract_interfaces("interface P { a: (Id | Blank)[]; }").expect("list");
        assert_eq!(ifaces[0].fields[0].ty, TypeShape::List(Box::new(union)));
    }

    /// **Every refused declaration is reported, not just the first.** This was
    /// asserted with two unions until unions parsed; the property belongs to
    /// [`extract_interfaces`]\'s error collection and not to unions, so it moves
    /// onto a form that is still refused rather than leaving with them.
    #[test]
    fn every_refused_field_is_reported_not_just_the_first() {
        let errors = extract_interfaces(
            "interface A { x: 1n; }\ninterface B { y: `abc`; }",
        )
        .expect_err("two refusals");
        assert_eq!(errors.len(), 2, "{errors:?}");
        // The refusal names the interface and the field, because the whole
        // point is that an author can find it.
        assert!(errors[0].contains("A") && errors[0].contains("x"), "{errors:?}");
        assert!(errors[1].contains("B") && errors[1].contains("y"), "{errors:?}");
        assert!(errors.iter().all(|e| e.is_ascii()), "{errors:?}");
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
            Some(&AttrValue::BindingExpr(BindingExpr::Literal(LiteralValue::Int64(2))))
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
                LiteralValue::String("hello".into())
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
                BindingExpr::Literal(LiteralValue::Bool(true)),
                BindingExpr::Path(vec!["props".into(), "count".into()]),
                BindingExpr::Null,
            ])))
        );
        assert_eq!(
            thing.attr("record"),
            Some(&AttrValue::BindingExpr(BindingExpr::Record(vec![
                (
                    "first".into(),
                    BindingExpr::Literal(LiteralValue::Int64(1))
                ),
                (
                    "second".into(),
                    BindingExpr::Literal(LiteralValue::String("two".into()))
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
                    BindingExpr::Literal(LiteralValue::String("fallback".into())),
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
    ///
    /// **`!=`, `!==` and `!` LEFT this list**, and where they went is pinned
    /// by [`both_inequality_operators_lower_and_keep_their_spelling`] and
    /// [`the_prefix_negation_lowers_as_the_form_the_author_wrote`]. The unary
    /// operators that stay refused replace `!a` here for the reason the
    /// inequalities were once named individually: `!x` lowering and `typeof x`
    /// not is one keystroke of difference, and an author told only
    /// "unary-operator" cannot tell which side of that line they are on.
    #[test]
    fn the_other_logical_operators_are_refused_by_name() {
        for (source, expected) in [
            (r#"<Thing value={a || b}/>"#, "`||`"),
            (r#"<Thing value={a && b}/>"#, "`&&`"),
            (r#"<Thing value={a > b}/>"#, "ordering-comparison"),
            (r#"<Thing value={a + b}/>"#, "binary-operator"),
            (r#"<Thing value={-a}/>"#, "`-`"),
            (r#"<Thing value={+a}/>"#, "`+`"),
            (r#"<Thing value={~a}/>"#, "`~`"),
            (r#"<Thing value={typeof a}/>"#, "`typeof`"),
            (r#"<Thing value={void a}/>"#, "`void`"),
            (r#"<Thing value={delete a.b}/>"#, "`delete`"),
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

    /// `design().isAuthoring` - the shape `MemberOf` exists for.
    ///
    /// Two facts in one: a BARE identifier call is captured as a
    /// `SymbolValue` (a namespace nobody spelled would be an invention, and a
    /// `Call` carrying three empty fields is the same expression written a
    /// second way), and the member access hangs off it rather than being folded
    /// into its name.
    #[test]
    fn a_member_chain_on_a_call_result_lowers_to_member_of() {
        assert_eq!(
            binding_of(r#"<Thing value={design().isAuthoring}/>"#),
            BindingExpr::MemberOf(
                Box::new(BindingExpr::SymbolValue(ObjectSymbol::Design)),
                PropertyAccessor::IsAuthoring,
            )
        );
        assert_eq!(
            binding_of(r#"<Thing value={design().avatar.sm.box}/>"#),
            BindingExpr::MemberOf(
                Box::new(BindingExpr::MemberOf(
                    Box::new(BindingExpr::MemberOf(
                        Box::new(BindingExpr::SymbolValue(ObjectSymbol::Design)),
                        PropertyAccessor::Named("avatar".into()),
                    )),
                    PropertyAccessor::Named("sm".into()),
                )),
                PropertyAccessor::Named("box".into()),
            ),
            "a deep chain NESTS one hop per node, outermost last"
        );
    }

    /// **An integral literal captures as `Int64`, a decimal one as `Float64`.**
    ///
    /// Capture width is decided by the TOKEN, never by the value and never by
    /// a declaration that has not arrived yet: `Int32`/`Float32` are what a
    /// field DECLARING them produces, through `LiteralValue::narrow` at
    /// lowering. A parser that guessed a narrow width from a small value would
    /// make `1` a different literal from `10000000000` for a reason the source
    /// does not state.
    #[test]
    fn an_integral_literal_captures_as_int64_and_a_decimal_one_as_float64() {
        for (source, expected) in [
            ("0", LiteralValue::Int64(0)),
            ("7", LiteralValue::Int64(7)),
            ("0x10", LiteralValue::Int64(16)),
            // **Past 2^53 the LEXER has already rounded**, so this reads the
            // source text: oxc's `value` for this token is
            // 4605617453661332480, a different integer with nothing to say so.
            ("4605617453661332513", LiteralValue::Int64(4605617453661332513)),
            ("1.5", LiteralValue::Float64(1.5)),
            // A DECIMAL POINT is part of the token, so `1.0` is a float even
            // though its value is whole. The alternative - reading the value -
            // would silently retype what the author wrote.
            ("1.0", LiteralValue::Float64(1.0)),
            ("1e3", LiteralValue::Float64(1000.0)),
        ] {
            assert_eq!(
                binding_of(&format!("<Thing value={{{source}}}/>")),
                BindingExpr::Literal(expected),
                "{source} captured at the wrong width",
            );
        }
    }

    /// **A written `null` is a source form, not a literal value.**
    ///
    /// It has no width and no type, which is what took it out of
    /// `LiteralValue`; what a consumer MAKES of it - absence, a refusal, a
    /// runtime null - stays the consumer's, and the consumers in this
    /// workspace already disagree.
    #[test]
    fn a_written_null_is_its_own_form() {
        assert_eq!(binding_of(r#"<Thing value={null}/>"#), BindingExpr::Null);
    }

    /// **A bare identifier call is a `SymbolValue`, and only a bare one.**
    ///
    /// The strictness is the whole matching rule: were `Call` also producible
    /// for `it()`, one authored spelling would have two shapes and every
    /// consumer would owe both an arm. Anything carrying a namespace, a type
    /// argument or an argument stays a `Call`, and `it` alone stays the NAME it
    /// is.
    #[test]
    fn a_bare_identifier_call_is_a_symbol_value() {
        assert_eq!(
            binding_of(r#"<Thing value={it()}/>"#),
            BindingExpr::SymbolValue(ObjectSymbol::It)
        );
        assert_eq!(
            binding_of(r#"<Thing value={frobnicate()}/>"#),
            BindingExpr::SymbolValue(ObjectSymbol::Named("frobnicate".into())),
            "an unregistered identifier is carried, never refused"
        );
        assert_eq!(
            binding_of(r#"<Thing value={it}/>"#),
            BindingExpr::Path(vec!["it".into()]),
            "a bare name is a path, not a call"
        );
        assert_eq!(
            binding_of(r#"<Thing value={ns.it()}/>"#),
            BindingExpr::Call {
                namespace: "ns".into(),
                name: "it".into(),
                type_args: vec![],
                args: vec![],
            },
            "a qualified call is somebody else's symbol"
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
                left: Box::new(BindingExpr::MemberOf(
                    Box::new(BindingExpr::SymbolValue(ObjectSymbol::Design)),
                    PropertyAccessor::Named("fidelity".into()),
                )),
                right: Box::new(BindingExpr::Literal(LiteralValue::String("lofi".into()))),
                strict: true,
            }
        );
        assert_eq!(
            binding_of(r#"<Thing value={props.kind == "row"}/>"#),
            BindingExpr::Eq {
                left: Box::new(path(&["props", "kind"])),
                right: Box::new(BindingExpr::Literal(LiteralValue::String("row".into()))),
                strict: false,
            },
            "a loose equality is recorded as a loose equality"
        );
    }

    /// **The prefix `!` lowers as the form the author wrote, and only that.**
    ///
    /// `!x` is a `Not`; so is `!(a == b)`, and the equality UNDER it stays an
    /// equality. That second clause is the direction this test owns: rewriting
    /// a negated equality into a [`BindingExpr::Ne`] on the way in would be as
    /// much of an invention as the reverse, and the emitter would then put
    /// back a `!=` nobody typed. The sibling test owns the other direction.
    ///
    /// The last case is the one a reader gets wrong: `!` binds tighter than
    /// `===`, so `!a === b` is an equality whose LEFT is a negation, not a
    /// negated equality. One bracket separates them and the capture keeps
    /// them apart.
    #[test]
    fn the_prefix_negation_lowers_as_the_form_the_author_wrote() {
        assert_eq!(
            binding_of(r#"<Thing value={!props.ready}/>"#),
            BindingExpr::Not(Box::new(path(&["props", "ready"])))
        );
        assert_eq!(
            binding_of(r#"<Thing value={!(a == b)}/>"#),
            BindingExpr::Not(Box::new(BindingExpr::Eq {
                left: Box::new(path(&["a"])),
                right: Box::new(path(&["b"])),
                strict: false,
            })),
            "a negated LOOSE equality keeps both halves of what was written"
        );
        assert_eq!(
            binding_of(r#"<Thing value={!(a === b)}/>"#),
            BindingExpr::Not(Box::new(BindingExpr::Eq {
                left: Box::new(path(&["a"])),
                right: Box::new(path(&["b"])),
                strict: true,
            }))
        );
        assert_eq!(
            binding_of(r#"<Thing value={!!a}/>"#),
            BindingExpr::Not(Box::new(BindingExpr::Not(Box::new(path(&["a"]))))),
            "a double negation is two nodes - cancelling them is a meaning"
        );
        assert_eq!(
            binding_of(r#"<Thing value={!a === b}/>"#),
            BindingExpr::Eq {
                left: Box::new(BindingExpr::Not(Box::new(path(&["a"])))),
                right: Box::new(path(&["b"])),
                strict: true,
            }
        );
    }

    /// **`!==` and `!=` lower to [`BindingExpr::Ne`], and NEITHER is a
    /// `Not(Eq)`.**
    ///
    /// The second clause is the load-bearing one, and it is ASSERTED rather
    /// than described because the alternative lowering is the attractive one:
    /// `Not(Eq { .. })` needs no new variant and every consumer would evaluate
    /// it identically. What it costs is the round trip - `a != b` would become
    /// indistinguishable from the `!(a == b)` an author could equally have
    /// written, so the emitter has to pick one spelling for both and the
    /// serialization stops being the element as authored.
    ///
    /// `strict` is carried for the reason [`BindingExpr::Eq`] carries it: the
    /// two operators differ exactly where coercion would happen.
    #[test]
    fn both_inequality_operators_lower_and_keep_their_spelling() {
        assert_eq!(
            binding_of(r#"<Thing value={design().fidelity !== "lofi"}/>"#),
            BindingExpr::Ne {
                left: Box::new(BindingExpr::MemberOf(
                    Box::new(BindingExpr::SymbolValue(ObjectSymbol::Design)),
                    PropertyAccessor::Named("fidelity".into()),
                )),
                right: Box::new(BindingExpr::Literal(LiteralValue::String("lofi".into()))),
                strict: true,
            }
        );
        assert_eq!(
            binding_of(r#"<Thing value={props.kind != "row"}/>"#),
            BindingExpr::Ne {
                left: Box::new(path(&["props", "kind"])),
                right: Box::new(BindingExpr::Literal(LiteralValue::String("row".into()))),
                strict: false,
            },
            "a loose inequality is recorded as a loose inequality"
        );
        for (inequality_source, negated_source) in [
            (
                r#"<Thing value={a != b}/>"#,
                r#"<Thing value={!(a == b)}/>"#,
            ),
            (
                r#"<Thing value={a !== b}/>"#,
                r#"<Thing value={!(a === b)}/>"#,
            ),
        ] {
            let inequality = binding_of(inequality_source);
            let negated = binding_of(negated_source);
            assert!(
                matches!(inequality, BindingExpr::Ne { .. }),
                "{inequality_source} lowered to {inequality:?}, not an inequality"
            );
            assert!(
                matches!(negated, BindingExpr::Not(_)),
                "{negated_source} lowered to {negated:?}, not a negation"
            );
            assert_ne!(
                inequality, negated,
                "{inequality_source} and {negated_source} are two authored \
                 forms and must not share one capture"
            );
        }
    }

    /// **Each negated spelling comes back out as the operator it went in
    /// with**, which is the whole reason there are two variants and not one.
    ///
    /// Asserted on the emitted TEXT, because that is where a fold would show:
    /// a lowering that turned `a != b` into `Not(Eq)` would still round-trip
    /// tree-to-tree (the sibling round-trip test would stay green) and would
    /// emit `!(a == b)` for a source that said `a != b`.
    #[test]
    fn a_negation_re_emits_as_the_operator_it_was_written_with() {
        for (source, expected) in [
            (r#"<Thing value={a != b}/>"#, "a != b"),
            (r#"<Thing value={a !== b}/>"#, "a !== b"),
            (r#"<Thing value={!(a == b)}/>"#, "!(a == b)"),
            (r#"<Thing value={!(a === b)}/>"#, "!(a === b)"),
            (r#"<Thing value={!props.ready}/>"#, "!props.ready"),
        ] {
            let doc = parse_tsx(source).expect("parse");
            let emitted = crate::emit::emit_tsx_document(&doc);
            assert!(
                emitted.contains(expected),
                "{source} should re-emit {expected:?}, emitted {emitted:?}"
            );
        }
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
            // The two negations. `!` binds tighter than every operator here,
            // so the cases that decide the emitter are the ones where its
            // operand is an operator (brackets REQUIRED, or the re-parse is a
            // negation of the left operand alone) and the ones where a
            // negation is an operand (brackets redundant and harmless).
            r#"<Thing value={!props.ready}/>"#,
            r#"<Thing value={!(a === b)}/>"#,
            r#"<Thing value={!(a ?? b)}/>"#,
            r#"<Thing value={!(a ? b : c)}/>"#,
            r#"<Thing value={!!a}/>"#,
            r#"<Thing value={!a === b}/>"#,
            r#"<Thing value={a !== b}/>"#,
            r#"<Thing value={props.kind != "row"}/>"#,
            r#"<Thing value={a != b ? c : d}/>"#,
            r#"<Thing value={!(a != b)}/>"#,
            r#"<Thing value={xs.map(x => !x.hidden)}/>"#,
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
            BlockStmt::Return(BindingExpr::Null)
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

    // ---------------------------------------------------------------
    // JSX text whitespace (`clean_jsx_text`) - the rule, from the spec.
    // ---------------------------------------------------------------

    /// Rules 1-7 exercised one at a time, on the raw text child that oxc hands
    /// `push_child`. `None` is rule 7: no text node at all.
    #[test]
    fn jsx_text_whitespace_follows_the_babel_rule() {
        // 7. nothing survives -> no node. The whitespace BETWEEN two elements
        //    on separate source lines, which is the commonest text child there
        //    is, and the reason pretty-printed TSX does not move.
        assert_eq!(clean_jsx_text("\n      "), None);
        assert_eq!(clean_jsx_text("\n"), None);
        assert_eq!(clean_jsx_text("\n\n   \n"), None);
        assert_eq!(clean_jsx_text(""), None);

        // 3 + 4 + 5: a child on its own indented line. First line empty and
        // dropped; middle line loses its indent (it is not the first) and is
        // the last non-empty one so gains no trailing space; final line empty
        // and dropped. Exactly what `trim()` gave.
        assert_eq!(
            clean_jsx_text("\n      A paragraph.\n    ").as_deref(),
            Some("A paragraph.")
        );

        // 6. surviving lines join with ONE space, however they were indented,
        //    and the last non-empty line gains none.
        assert_eq!(
            clean_jsx_text("\n  one\n     two\n  three\n").as_deref(),
            Some("one two three")
        );

        // 4. the LAST line keeps its trailing spaces - this is the bug's fix.
        //    `When ` before an element child arrives as "\n  When ".
        assert_eq!(clean_jsx_text("\n  When ").as_deref(), Some("When "));
        // and the text AFTER the element arrives as " in the table.\n".
        assert_eq!(
            clean_jsx_text(" in the table.\n").as_deref(),
            Some(" in the table.")
        );

        // 3. the FIRST line keeps its leading spaces.
        assert_eq!(clean_jsx_text("  a\n  b").as_deref(), Some("  a b"));

        // 2. tabs are spaces, and a tab-only line is empty like a space-only
        //    one.
        assert_eq!(clean_jsx_text("\n\ta\tb\n").as_deref(), Some("a b"));
        assert_eq!(clean_jsx_text("\n\t\n").is_none(), true);

        // 1. all three line breaks split, and \r\n counts once.
        assert_eq!(clean_jsx_text("\r\n  a\r\n  b\r\n").as_deref(), Some("a b"));
        assert_eq!(clean_jsx_text("\r  a\r  b\r").as_deref(), Some("a b"));
    }

    /// The single-line child is where the rule and `trim()` part company, and
    /// it is deliberate: one line is BOTH the first and the last, so neither
    /// strip applies and the padding is the author's.
    #[test]
    fn a_single_line_text_child_keeps_its_own_padding() {
        assert_eq!(clean_jsx_text(" Hello ").as_deref(), Some(" Hello "));
        assert_eq!(clean_jsx_text("Hello").as_deref(), Some("Hello"));
        // A lone space between two elements on ONE source line is a real word
        // boundary and survives; the same gap spread over lines does not.
        assert_eq!(clean_jsx_text(" ").as_deref(), Some(" "));
        assert_eq!(clean_jsx_text(" \n "), None);
    }

    /// Interior whitespace is never touched - only the runs that touch a line
    /// break are. A doubled space inside a sentence is the author's.
    #[test]
    fn interior_whitespace_is_the_authors() {
        assert_eq!(
            clean_jsx_text("\n  a  b   c\n").as_deref(),
            Some("a  b   c")
        );
    }

    /// The third place the rules differ, and the one nothing in the corpus
    /// exercises: prose that spans source lines. `trim()` only touched the
    /// ENDS, so the newline and the following indent survived INSIDE the
    /// string and reached the draw pass as characters; the rule collapses each
    /// break to the single space a reader sees.
    #[test]
    fn prose_wrapped_across_source_lines_joins_with_one_space() {
        let raw = "two\n  lines";
        assert_eq!(raw.trim(), "two\n  lines", "what trim() used to hand on");
        assert_eq!(clean_jsx_text(raw).as_deref(), Some("two lines"));
    }

    /// The bug, end to end: the space beside an element child now reaches the
    /// tree, on one source line and on three, and `{" "}` produces the same
    /// text so the escape hatch and the space are interchangeable.
    #[test]
    fn a_space_beside_an_element_child_survives_the_parse() {
        let one_line =
            parse_tsx(r#"<Content>When <Text id="a" /> in the Water Logs table.</Content>"#)
                .expect("parse");
        let Node::Element(content) = &one_line.root_nodes[0] else { panic!("expected element") };
        assert_eq!(content.children[0], Node::Text("When ".into()));
        assert!(matches!(content.children[1], Node::Element(_)));
        assert_eq!(
            content.children[2],
            Node::Text(" in the Water Logs table.".into())
        );

        // The SAME authored text with the element on its own line still welds,
        // and that is the rule working rather than the bug surviving: a line
        // break between text and an element is not a space in JSX (rules 3-4
        // strip exactly the whitespace that touches a break). The space has to
        // be on the same source line as the element, which is the JSX author's
        // long-standing rule and the reason `{" "}` exists at all.
        let three_lines = parse_tsx(
            "<Content>\n  When\n  <Text id=\"a\" />\n  in the Water Logs table.\n</Content>",
        )
        .expect("parse");
        let Node::Element(split) = &three_lines.root_nodes[0] else { panic!() };
        assert_eq!(split.children[0], Node::Text("When".into()));
        assert_eq!(
            split.children[2],
            Node::Text("in the Water Logs table.".into())
        );

        // And the escape hatch it replaces yields the identical children.
        let hatch = parse_tsx(
            r#"<Content>When{" "}<Text id="a" />{" "}in the Water Logs table.</Content>"#,
        )
        .expect("parse");
        let Node::Element(hatched) = &hatch.root_nodes[0] else { panic!() };
        let joined = |el: &Element| {
            el.children
                .iter()
                .filter_map(|n| match n {
                    Node::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("|")
        };
        assert_eq!(joined(content), "When | in the Water Logs table.");
        assert_eq!(joined(hatched), "When| | |in the Water Logs table.");
        // Same characters in the same order once the run boundaries are gone -
        // which is what the rendered frame sees.
        let flat = |el: &Element| {
            el.children
                .iter()
                .filter_map(|n| match n {
                    Node::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<String>()
        };
        assert_eq!(flat(content), flat(hatched));
    }

    /// The pretty-printed corpus MUST NOT MOVE. Every shape the authored `.tsx`
    /// files actually use, asserted to agree with what `trim()` gave.
    #[test]
    fn pretty_printed_children_are_unchanged_by_the_new_rule() {
        for raw in [
            "\n    ",
            "\n        ",
            "Hi",
            "Decorated runs are addressed",
            "\n    A paragraph on its own line.\n    ",
            "\n\n    ",
            "\n            two words\n        ",
        ] {
            let trimmed = raw.trim();
            let cleaned = clean_jsx_text(raw);
            assert_eq!(
                cleaned.as_deref().unwrap_or(""),
                trimmed,
                "the new rule moved a pretty-printed child: {raw:?}"
            );
        }
    }
}
