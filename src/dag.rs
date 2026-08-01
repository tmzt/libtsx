//! `DagNode` — the serializable code-graph contract (semantic AST).
//!
//! This is the **single owned boundary type** between the TSX/TypeScript world
//! and everything downstream (nocap-witgen, highbay-build, the language
//! switcher). No `oxc_*` type appears here or anywhere in this module's public
//! API: consumers get plain, serde-serializable Rust data.
//!
//! Scope (deliberately minimal, forward-compatible):
//! * **The element tree** — [`Element`], [`Node`], [`AttrValue`] and the
//!   whole-document [`TsxDocument`] that holds it. The authored UI itself:
//!   tag, type arguments, attributes in source order, children in source
//!   order.
//! * **TS `interface` declarations** — name, fields, optionality, nested
//!   shapes ([`InterfaceDecl`], [`TypeShape`]). These are the Props shapes
//!   that project into WIT records and generated forms.
//! * **`import` edges** ([`ImportDecl`]) — the typed references a module makes
//!   to the providers it consumes.
//! * **Simple event-handler ops** ([`HandlerDecl`], [`Stmt`], [`Expr`]) — the
//!   restricted semantic AST that projects symmetrically across language
//!   views and synthesizes directly to wasm. Complex Modules are *not*
//!   represented here; they are opaque native-language units by design.
//! * **Effect bindings** ([`NamedEffect`], [`AttrValue::NamedEffect`],
//!   [`HostEffects`]) - `onTap={navigate("Chat")}`: an [`is_event_binding`]
//!   attribute whose value is ONE call resolving to a granted host import.
//!   Deliberately *not* a handler and not a body - see [`NamedEffect`] and
//!   [`HOST_PREFIX`] (LIBHBUI_PLAN Rules 46, 46a, 48).
//!
//! Every type here is serde-serializable and free of parser dependencies:
//! `dag` is available with `default-features = false`, so a consumer that only
//! reads and writes the graph never links oxc. The oxc-backed [`crate::parse`]
//! module *produces* these values; it does not own any of them.
//!
//! **Why the element tree lives here and not in `parse`.** It is the same
//! reason [`ImportDecl`] does: what the parser yields is graph data, and a
//! graph type that only exists when the parser feature is on is a graph type
//! half the stack cannot name. The serialized Highbay format *is* this module
//! in postcard form (LIBHBUI_PLAN Rule 20), and a definition exposes exactly
//! one [`Element`] as its UI (Rule 18) — both of which require the element
//! tree to be a `dag` type, not a parse-result type.

use serde::{Deserialize, Serialize};

/// A node in the code graph. The umbrella type consumed by `nocap-witgen`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DagNode {
    /// A whole module: interfaces + handlers + host imports.
    Module(DagModule),
    /// A single TS `interface` declaration.
    Interface(InterfaceDecl),
    /// A single event-handler function.
    Handler(HandlerDecl),
}

/// A module of code: the unit `highbay-build` feeds to witgen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagModule {
    /// Module name (used for the WIT package/interface identity).
    pub name: String,
    /// TS `interface` declarations (Props shapes).
    pub interfaces: Vec<InterfaceDecl>,
    /// Host import **signatures**: what may be called, and with which
    /// parameters (nav edges, actions, nocap ops).
    ///
    /// **Not the same concept as [`ImportDecl`], despite the shared word.**
    /// An [`ImportDecl`] is the authored `import` statement - *where a name
    /// came from*: a specifier plus bindings with `local`/`imported`/`kind`.
    /// This is *what a name may be called as*. An effect needs BOTH: the
    /// declaration binds the local name, this types the call. Resolving a call
    /// against the wrong one is a defect, not a shortcut - see
    /// `parse::EffectScope`, which walks the declaration to a namespace and an
    /// exported name and only then asks the grant for the signature.
    pub imports: Vec<FuncSig>,
    /// Event handlers.
    ///
    /// **Empty in everything the authoring surface produces, and that is
    /// correct rather than a gap** (LIBHBUI_PLAN Rule 46a): an effect is one
    /// call expression in an `on[A-Z]*` attribute, so there is no handler to
    /// declare. `HandlerDecl`, `Stmt`, `Return`, `If` and `Set` are a codec
    /// shape the format carries; nothing parses one, and
    /// `Definition::from_document` writes `handlers: Vec::new()`.
    pub handlers: Vec<HandlerDecl>,
}

/// A TS `interface` declaration: `interface Name { fields… }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

/// An ES `import { a, b as c } from "src"` declaration — the **typed reference**
/// edge from a module to a provider it consumes (a screen importing a Script's
/// exported source, e.g. a `ListAdapter` provider). Owned + serde; no `oxc_*`
/// type appears here. The semantic AST records the edge (what a module depends
/// on and under what local name); resolving it to a concrete provider is the
/// consumer's job (highbay_ui's provider registry today; a real module runtime
/// later).
///
/// **This is *where a name came from*, and nothing more.** It carries no
/// signature: what the name may be *called as* lives in a [`FuncSig`] - in
/// [`HostEffects`] for a granted host namespace, in [`DagModule::imports`] once
/// a module is assembled. The two are chained, never interchangeable: a local
/// name resolves through this declaration to a namespace and an exported name,
/// and only then to the signature that types the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportDecl {
    /// The module specifier string (`from "…"`) — a Script's display name in the
    /// Highbay module system, or a real package path.
    pub source: String,
    /// The imported bindings, in source order.
    pub names: Vec<ImportName>,
}

/// One imported binding of an [`ImportDecl`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportName {
    /// The local name this module refers to the binding by.
    pub local: String,
    /// The name exported by the source module. Equals `local` for a default or
    /// namespace import; the `imported` half of `imported as local` otherwise.
    pub imported: String,
    /// Which import form introduced the binding.
    pub kind: ImportKind,
}

/// The three import binding forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    /// `import { imported as local }` (or `import { name }`).
    Named,
    /// `import local from "src"`.
    Default,
    /// `import * as local from "src"`.
    Namespace,
}

/// A parsed JSX element with ordered attributes and children — **the authored
/// UI itself**, and the one node kind a definition exposes as its UI
/// (LIBHBUI_PLAN Rule 18).
///
/// Nothing here is filtered or normalized on the way in: attributes keep source
/// order, children keep source order, and a generic element keeps its type
/// arguments. A serialization of an `Element` is therefore the element as
/// authored, which is what makes graph -> TSX a real direction rather than an
/// aspiration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Element {
    /// The tag name (e.g. `List`, `Content`, `A.B` for member tags).
    pub tag: String,
    /// The element's **type arguments** — the `Message` in `<List<Message>>`
    /// (TypeScript's generic JSX form, TS 2.9+), in source order. Empty for the
    /// ordinary non-generic spelling, which is every element that does not
    /// write one.
    ///
    /// Lowered through the same [`TypeShape`] mapping an `interface` field's
    /// type takes, so `<List<Message>>` is `[TypeShape::Named("Message")]` and
    /// `<List<string>>` is `[TypeShape::String]` — one type vocabulary, not a
    /// second one for the type-argument position.
    ///
    /// **Why this is parsed rather than ignored.** A generic element's type
    /// argument is the author saying what flows through it; dropping it here
    /// means it exists in the source and nowhere else, which is the whole
    /// failure mode `Element` is supposed to prevent. Consumers that do not
    /// model generics simply see an empty `Vec`.
    pub type_args: Vec<TypeShape>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// `onTap={navigate("Chat")}` - an **effect binding** (LIBHBUI_PLAN
    /// Rules 46, 46a, 48).
    ///
    /// The attribute name matched [`is_event_binding`], so the author declared
    /// an event binding; the value was one call expression resolving to a
    /// declared host import. See [`NamedEffect`].
    ///
    /// **Appended last on purpose.** postcard encodes enum variants
    /// positionally, so a variant inserted anywhere else would renumber every
    /// value above it in already-written bytes.
    NamedEffect(NamedEffect),
}

/// The prefix that marks a module specifier as a **host namespace**: an import
/// with no source (LIBHBUI_PLAN Rule 48).
///
/// [`ImportDecl::source`] otherwise offers two cases, and both imply something
/// resolvable to source - "a Script's display name in the Highbay module
/// system, or a real package path". A host namespace is a third: there is no
/// file, no compiled Script and nothing to resolve to. A Script import is
/// *compiled*; a host import is *granted*, and this prefix is how the parse
/// tells them apart before it tries to do either.
pub const HOST_PREFIX: &str = "host:";

/// Whether a module specifier names a host namespace at all - spelled with
/// [`HOST_PREFIX`], whether or not anything grants it.
pub fn is_host_namespace(source: &str) -> bool {
    source.starts_with(HOST_PREFIX)
}

/// Whether an attribute name announces an **event binding**: `on` followed by
/// an upper-case letter (LIBHBUI_PLAN Rule 46a).
///
/// Mechanical, and that is the whole point: there is no reserved-name list to
/// maintain and no per-tag allowlist to keep in step with the control set. An
/// attribute matching this *says* it is an event binding, so a value that
/// cannot be one is a mistake the author declared rather than a shape something
/// has to know about in advance.
///
/// `onTap` and `onLongPress` match; `on`, `once`, `only` and `column` do not.
pub fn is_event_binding(attr: &str) -> bool {
    attr.strip_prefix("on")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// An **effect**: a declared host import called with arguments
/// (LIBHBUI_PLAN Rules 46a, 48).
///
/// **"Named" is load-bearing.** An effect is never anonymous: [`name`] is a
/// host import's own exported name, resolved through the module's import chain
/// and checked against the [`FuncSig`] that namespace declares. A carrier
/// holding an arbitrary [`Expr`] would admit an anonymous effect and lose that
/// check; a named one cannot be written without something to resolve to.
///
/// [`name`]: NamedEffect::name
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedEffect {
    /// The host import's **exported** name - the `imported` half of the import
    /// chain, so `import { navigate as go }` and a call to `go(..)` both arrive
    /// here as `navigate`. An alias is resolved once, at parse, rather than at
    /// every reader.
    pub name: String,
    /// The call's arguments, in source order, lowered against the declared
    /// parameter types. Literals only - an effect is not an expression
    /// language (Rule 46a).
    pub args: Vec<Expr>,
}

/// The host imports a load **grants**: namespace -> the signatures it declares
/// (LIBHBUI_PLAN Rule 48).
///
/// Not graph data and deliberately not serialized. A grant is what the *host*
/// offers a source, so it arrives from the embedding rather than out of the
/// document - which is the difference between a Script import (compiled) and a
/// host import (granted). `libhbui` declares the one it implements.
///
/// **What this does not know.** Whether a non-host specifier names a real
/// project Script is the consumer's question, not the parser's, so a
/// non-`host:` import is left alone here exactly as it always has been. What
/// *is* checkable at parse - and now is - is that a `host:` specifier names a
/// granted namespace, that its bindings are named imports of signatures the
/// namespace declares, and that a call through one matches its signature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostEffects {
    granted: Vec<(String, Vec<FuncSig>)>,
}

impl HostEffects {
    /// Nothing granted: every `host:` import is refused, and every effect call
    /// is unresolved.
    ///
    /// The honest default rather than a lenient one. A source that calls an
    /// effect nothing granted it has named a capability it was not given, and
    /// that is a refusal at the point it is written.
    pub fn none() -> Self {
        Self::default()
    }

    /// One granted namespace and the imports it declares.
    ///
    /// # Panics
    ///
    /// If `namespace` is not spelled as a host namespace ([`HOST_PREFIX`]) - a
    /// grant that no import could ever name is a mistake in the embedding, and
    /// a silent one would look exactly like an effect that does not resolve.
    pub fn granting(namespace: impl Into<String>, imports: Vec<FuncSig>) -> Self {
        let mut host = Self::none();
        host.grant(namespace, imports);
        host
    }

    /// Grant another namespace. See [`HostEffects::granting`] for the panic.
    pub fn grant(&mut self, namespace: impl Into<String>, imports: Vec<FuncSig>) {
        let namespace = namespace.into();
        assert!(
            is_host_namespace(&namespace),
            "`{namespace}` is granted as a host namespace and is not spelled as one (`{HOST_PREFIX}...`)"
        );
        self.granted.push((namespace, imports));
    }

    /// The signatures a granted namespace declares, or `None` if nothing
    /// grants it.
    pub fn namespace(&self, source: &str) -> Option<&[FuncSig]> {
        self.granted
            .iter()
            .find(|(name, _)| name == source)
            .map(|(_, sigs)| sigs.as_slice())
    }

    /// The signature a granted namespace declares under this exported name.
    pub fn declares(&self, namespace: &str, name: &str) -> Option<&FuncSig> {
        self.namespace(namespace)?.iter().find(|s| s.name == name)
    }
}

/// Why an effect binding cannot mean what it says (LIBHBUI_PLAN Rules 46a, 48).
///
/// Every variant is a **declaration** that is wrong, and every one of them is a
/// refusal rather than an `AttrValue::Opaque` that silently does nothing. That
/// is the whole reason detection and carriage are one step: an attribute
/// matching [`is_event_binding`] announces itself, so there is no case in which
/// "we could not lower this" and "the author wrote no effect" arrive as the
/// same value (Rule 10).
#[derive(Debug, Clone, PartialEq)]
pub enum EffectError {
    /// An `on..` attribute whose value is not a call: a bare identifier, a
    /// string, an arrow function, a block.
    NotACall {
        /// The attribute that announced an effect.
        attr: String,
    },
    /// The callee resolves, through the local->imported chain, to no declared
    /// host import.
    Unresolved {
        /// The attribute that announced an effect.
        attr: String,
        /// The callee as written.
        callee: String,
    },
    /// The call supplies a different number of arguments than the signature
    /// declares.
    ArgCount {
        /// The attribute that announced an effect.
        attr: String,
        /// The host import's exported name.
        effect: String,
        /// How many parameters the signature declares.
        declared: usize,
        /// How many arguments the call supplies.
        given: usize,
    },
    /// An argument is a literal of a kind the declared parameter type cannot
    /// hold - a string where an `S32` is declared, or the reverse.
    ArgType {
        /// The attribute that announced an effect.
        attr: String,
        /// The host import's exported name.
        effect: String,
        /// Which argument, counting from zero.
        index: usize,
        /// The parameter type the signature declares.
        declared: TypeShape,
    },
    /// An argument is not a literal at all. An effect call is not an
    /// expression language: anything wanting a computation is a Module,
    /// referenced opaquely (Rule 46a).
    ArgNotALiteral {
        /// The attribute that announced an effect.
        attr: String,
        /// The host import's exported name.
        effect: String,
        /// Which argument, counting from zero.
        index: usize,
    },
    /// A host namespace bound by `import * as fx` or `import fx from`.
    ///
    /// `fx.navigate(..)` is a member expression and [`Expr::Call`]'s callee is a
    /// flat `String`; encoding `"fx.navigate"` into it would be structure
    /// smuggled into a name. Named imports only, until a callee carries a path
    /// properly.
    NotANamedImport {
        /// The host namespace.
        source: String,
        /// The local name it was bound to.
        local: String,
        /// The import form that bound it.
        kind: ImportKind,
    },
    /// An import from a `host:` namespace nothing granted - neither a project
    /// Script nor a declared host namespace, so there is nothing for it to be.
    UnknownHostNamespace {
        /// The specifier as written.
        source: String,
    },
    /// A host namespace is granted and does not declare this name.
    UndeclaredHostImport {
        /// The host namespace.
        source: String,
        /// The exported name the import asked for.
        imported: String,
    },
    /// The source imports a `host:` namespace and the parse context does not
    /// offer the effect surface at all (Rule 49's `enable_effects`).
    ///
    /// Distinct from [`EffectError::UnknownHostNamespace`], which is a load
    /// that offers effects and does not grant *this* one. The two say different
    /// things to whoever reads the message - one is a capability the embedding
    /// withheld, the other is an embedding that has no capabilities to give -
    /// and collapsing them was how the default context's refusal came to blame
    /// the source for a decision the caller made.
    EffectsNotOffered {
        /// The specifier as written.
        source: String,
    },
    /// A JSX **spread attribute** - `<Action {...handlers}/>`.
    ///
    /// Refused rather than skipped, and the reason is Rule 46a's: a spread's
    /// contents are not statically known, so `{...handlers}` where
    /// `handlers = { onTap: navigate("Chat") }` would reach an element as *no
    /// attribute at all*. The attribute loop never sees an `on..` name, so
    /// every refusal above is blind to it, and the effect is erased by exactly
    /// the route the [`AttrValue::NamedEffect`] producer exists to close.
    ///
    /// It could not be honoured even if it were resolvable: an attribute set
    /// spread from a value cannot be checked against declared props, so
    /// accepting one would be a second, unchecked way to give an element
    /// attributes.
    SpreadAttribute {
        /// The tag it was written on.
        tag: String,
    },
}

impl std::fmt::Display for EffectError {
    /// ASCII only - these strings reach logs, panic dumps and the editor's
    /// live-parse status strip.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotACall { attr } => write!(
                f,
                "`{attr}` is an event binding, so its value must be one call returning an effect"
            ),
            Self::Unresolved { attr, callee } => write!(
                f,
                "`{attr}` calls `{callee}`, which is not an imported host effect"
            ),
            Self::ArgCount {
                attr,
                effect,
                declared,
                given,
            } => write!(
                f,
                "`{attr}` calls `{effect}` with {given} arguments and it declares {declared}"
            ),
            Self::ArgType {
                attr,
                effect,
                index,
                declared,
            } => write!(
                f,
                "`{attr}`: argument {index} of `{effect}` is declared {declared:?} and is not one"
            ),
            Self::ArgNotALiteral {
                attr,
                effect,
                index,
            } => write!(
                f,
                "`{attr}`: argument {index} of `{effect}` is not a literal, and an effect call is not an expression language"
            ),
            Self::NotANamedImport {
                source,
                local,
                kind,
            } => write!(
                f,
                "`{source}` is a host namespace and `{local}` binds it as {kind:?}; effects are named imports only"
            ),
            Self::UnknownHostNamespace { source } => write!(
                f,
                "`{source}` names no granted host namespace, and there is no source to compile"
            ),
            Self::UndeclaredHostImport { source, imported } => write!(
                f,
                "the host namespace `{source}` declares no `{imported}`"
            ),
            Self::EffectsNotOffered { source } => write!(
                f,
                "`{source}` is a host namespace and this load does not offer effects"
            ),
            Self::SpreadAttribute { tag } => write!(
                f,
                "<{tag}> spreads its attributes, and a spread cannot be resolved to declared props or checked for an effect"
            ),
        }
    }
}

impl std::error::Error for EffectError {}

/// A node in a parsed JSX tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// A parsed, fully-owned TSX document (no oxc arena references): the element
/// tree a source file contributes, plus the typed reference edges it declares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TsxDocument {
    pub root_nodes: Vec<Node>,
    /// Top-level `import … from "…"` declarations, in source order — the typed
    /// reference edges from this module to the providers it consumes (empty for
    /// a bare `<JSX/>` document). See [`ImportDecl`].
    pub imports: Vec<ImportDecl>,
}

// --- the definition ---------------------------------------------------------

/// The reserved attribute naming a definition's props shape, read on the
/// **outer container only** (LIBHBUI_PLAN Rule 41).
pub const RESERVED_ATTR_PROPS: &str = "props";

/// The reserved attribute carrying a stored widget id (LIBHBUI_PLAN Rule 40).
///
/// Reserved everywhere in the dialect; special-cased (read as the definition's
/// own identity) on the outer container only. `dag` does not interpret the
/// value - the id type belongs to the consumer - it only reserves the name so
/// nothing else can claim it.
pub const RESERVED_ATTR_ID: &str = "id";

/// The reserved record-key attribute, reserved the same way as
/// [`RESERVED_ATTR_ID`] (LIBHBUI_PLAN Rule 41). A record key is opaque: hashed
/// and compared, never interpreted, so nothing here parses it.
pub const RESERVED_ATTR_KEY: &str = "key";

/// A **definition**: the unit one screen or widget source declares - exactly
/// one exposed [`Element`] plus the other AST nodes alongside it
/// (LIBHBUI_PLAN Rules 17, 18).
///
/// **Why this type exists.** [`TsxDocument`] carries the element tree and the
/// import edges, but not the interfaces: `parse_tsx` and `extract_interfaces`
/// are separate entry points, so a definition's parts arrived from two places
/// and no single type stood for the whole of one. That gap is closed here,
/// which is where it belongs: the relevant-AST set is libtsx's to define, and
/// growing it is a change to libtsx (Rule 21).
///
/// **The one-child rule is structural, not a check.** [`ui`](Definition::ui)
/// is one `Element` and not a `Vec`, so a second exposed element has nowhere
/// to live: the rule is unrepresentable to violate rather than rejected at
/// runtime. Interfaces, imports and handlers are not elements, so they sit
/// alongside without competing for that slot - which is exactly what the slot
/// being typed `Element` (rather than the whole node list being called "the
/// UI") buys.
///
/// **The symbol is stored, never derived** (Rule 9). [`symbol`](Definition::
/// symbol) is the exported name other sources refer to this definition by -
/// the `UserCard` in `<UserCard/>`. Nothing here reconstructs it from a
/// display name, a title, a file name or a position, and there is deliberately
/// no helper that would: renaming a display must not change what `<UserCard/>`
/// resolves to. Callers supply it from wherever the authoring step recorded
/// it.
///
/// Everything a definition is made of is already a `dag` type, so a definition
/// serializes with the rest of the graph and needs no format of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    /// The exported symbol other sources refer to this definition by. Stored,
    /// never derived (Rule 9).
    pub symbol: String,
    /// The single [`Element`] this definition exposes as its UI (Rule 18) -
    /// the outer container, with its attributes and children exactly as
    /// authored.
    pub ui: Element,
    /// The `interface` declarations alongside the UI - the props shapes.
    /// [`Definition::props_shape`] resolves the one the container names.
    pub interfaces: Vec<InterfaceDecl>,
    /// The typed reference edges this source declares, in source order.
    pub imports: Vec<ImportDecl>,
    /// The simple event handlers alongside the UI.
    pub handlers: Vec<HandlerDecl>,
}

impl Definition {
    /// Assemble a definition from the two halves libtsx's parser hands back:
    /// a [`TsxDocument`] (element tree + import edges) and the interfaces
    /// extracted from the same source.
    ///
    /// The document's root nodes are the loose shape - a `Vec<Node>` that can
    /// hold any number of anything - and this is where that shape narrows to
    /// the one element a definition exposes. An error here says the *source*
    /// was not a definition; it is not a check on the type, which cannot hold
    /// two elements in the first place (Rule 18).
    ///
    /// `handlers` starts empty: libtsx has no handler-extraction entry point
    /// yet, and inventing one by inference is exactly the derivation Rule 30
    /// forbids. Callers with handlers assign the field.
    pub fn from_document(
        symbol: impl Into<String>,
        doc: TsxDocument,
        interfaces: Vec<InterfaceDecl>,
    ) -> Result<Self, DefError> {
        let TsxDocument { root_nodes, imports } = doc;
        let mut roots = root_nodes.into_iter();
        let Some(first) = roots.next() else {
            return Err(DefError::NoUi);
        };
        let extra = roots.count();
        if extra > 0 {
            // Not "take the first and drop the rest": a source with two roots
            // has content that would silently vanish, and a definition whose
            // UI is quietly half of what was written is worse than one that
            // refuses to be built.
            return Err(DefError::SeveralRoots(1 + extra));
        }
        let Node::Element(ui) = first else {
            return Err(DefError::RootNotAnElement);
        };
        Ok(Self {
            symbol: symbol.into(),
            ui,
            interfaces,
            imports,
            handlers: Vec::new(),
        })
    }

    /// The props shape the outer container names, as written.
    ///
    /// This is one of the two attributes the outer container special-cases
    /// (Rule 41), and the special-casing goes no further: no other element in
    /// the tree gets its attributes interpreted here.
    ///
    /// `Ok(None)` means no props shape is declared. A declaration in a form
    /// that is not a name is an error rather than a `None`, because "declared
    /// unreadably" and "not declared" are different facts and must not arrive
    /// as the same value (Rule 10).
    pub fn props_name(&self) -> Result<Option<&str>, DefError> {
        match self.ui.attr(RESERVED_ATTR_PROPS) {
            None => Ok(None),
            // `props="Shape"` and `props={Shape}` are the two spellings a name
            // arrives in; both are read, neither is guessed at.
            Some(AttrValue::Str(name)) | Some(AttrValue::Binding(name)) => Ok(Some(name.as_str())),
            Some(_) => Err(DefError::ReservedAttrNotAName(RESERVED_ATTR_PROPS)),
        }
    }

    /// The interface [`Definition::props_name`] names, resolved against the
    /// interfaces declared alongside the UI.
    ///
    /// A name that resolves to nothing is an error, not a `None`: a props
    /// shape the author declared and this definition cannot find is a missing
    /// declaration, and reporting it as "no props" would hide it.
    pub fn props_shape(&self) -> Result<Option<&InterfaceDecl>, DefError> {
        let Some(name) = self.props_name()? else {
            return Ok(None);
        };
        self.interfaces
            .iter()
            .find(|i| i.name == name)
            .map(Some)
            .ok_or_else(|| DefError::UnknownPropsShape(name.to_string()))
    }
}

/// What can be wrong with a definition's *source*. None of these is a check on
/// [`Definition`] itself, whose shape makes the one-child rule unrepresentable
/// to violate (Rule 18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefError {
    /// The source contributed no root node at all.
    NoUi,
    /// The source contributed more than one root node; a definition exposes
    /// exactly one element (Rule 18). Carries how many were found.
    SeveralRoots(usize),
    /// The single root node was text or an expression, not an element.
    RootNotAnElement,
    /// A reserved attribute on the outer container was declared in a form that
    /// is not a name (Rule 41).
    ReservedAttrNotAName(&'static str),
    /// `props` names a shape no interface alongside this definition declares.
    UnknownPropsShape(String),
}

impl std::fmt::Display for DefError {
    /// ASCII only - these strings reach logs and panic dumps.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoUi => write!(f, "the source declares no UI element"),
            Self::SeveralRoots(n) => write!(
                f,
                "a definition exposes exactly one element, the source has {n} root nodes"
            ),
            Self::RootNotAnElement => write!(f, "the source's only root node is not an element"),
            Self::ReservedAttrNotAName(attr) => {
                write!(f, "the reserved `{attr}` attribute is not a name")
            }
            Self::UnknownPropsShape(name) => {
                write!(f, "no interface named `{name}` is declared alongside")
            }
        }
    }
}

impl std::error::Error for DefError {}

/// One field of an interface (or one named function parameter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeShape,
    /// TS `name?: T` optionality.
    #[serde(default)]
    pub optional: bool,
}

/// The shape of a type as it crosses the edge. Maps 1:1 onto WIT types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypeShape {
    Bool,
    /// TS `number` annotated as 32-bit integer.
    S32,
    /// TS `number`/`bigint` annotated as 64-bit integer.
    S64,
    F32,
    /// TS `number` (default lowering).
    F64,
    String,
    /// `T[]` / `Array<T>`.
    List(Box<TypeShape>),
    /// `T | undefined` / optional shapes used positionally.
    Option(Box<TypeShape>),
    /// An inline anonymous object shape (`{ a: number }`); witgen hoists
    /// these into named records.
    Record(Vec<FieldDecl>),
    /// A reference to another interface by name.
    Named(String),
}

/// A function signature (handler export or host import).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuncSig {
    pub name: String,
    pub params: Vec<FieldDecl>,
    /// `None` = no return value.
    #[serde(default)]
    pub result: Option<TypeShape>,
}

/// An event handler: a signature plus a *simple* body.
///
/// Bodies are restricted to the semantic-AST op set ([`Stmt`]/[`Expr`]) so
/// they can project across language views and synthesize directly to wasm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerDecl {
    pub sig: FuncSig,
    pub body: Vec<Stmt>,
}

/// Simple statement ops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Stmt {
    /// Evaluate an expression for its effect (host calls).
    Expr(Expr),
    /// `return;` / `return expr;`
    Return(Option<Expr>),
    /// `if (cond) { … } else { … }`
    If {
        cond: Expr,
        then_branch: Vec<Stmt>,
        #[serde(default)]
        else_branch: Vec<Stmt>,
    },
    /// Property write through the flat nocap ABI (`setProperty`).
    Set { path: String, value: Expr },
}

/// Simple expression ops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    LitBool(bool),
    LitS32(i32),
    LitS64(i64),
    LitF32(f32),
    LitF64(f64),
    LitStr(String),
    /// Reference to a handler parameter by index.
    Param(u32),
    /// Property read through the flat nocap ABI (`getProperty`).
    Get { path: String },
    /// Binary operation.
    Bin {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Call a declared host import (nav edge, action trigger, …).
    Call { callee: String, args: Vec<Expr> },
}

/// Binary operators available to simple handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Logical and — note: synthesized non-short-circuit (both sides eval).
    And,
    /// Logical or — note: synthesized non-short-circuit (both sides eval).
    Or,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_module() -> DagModule {
        DagModule {
            name: "counter".into(),
            interfaces: vec![InterfaceDecl {
                name: "CounterProps".into(),
                fields: vec![
                    FieldDecl { name: "label".into(), ty: TypeShape::String, optional: false },
                    FieldDecl { name: "count".into(), ty: TypeShape::S32, optional: false },
                    FieldDecl {
                        name: "history".into(),
                        ty: TypeShape::List(Box::new(TypeShape::S32)),
                        optional: true,
                    },
                    FieldDecl {
                        name: "style".into(),
                        ty: TypeShape::Record(vec![FieldDecl {
                            name: "bold".into(),
                            ty: TypeShape::Bool,
                            optional: false,
                        }]),
                        optional: false,
                    },
                ],
            }],
            // `navigate` takes the destination's STORED SYMBOL (LIBHBUI_PLAN
            // Rule 9), so its parameter is a string. It read `S32` here until
            // the effect producer landed, which disagreed with every real
            // destination in the system - `navigate(0)` names nothing.
            //
            // WHAT IS CHECKED, AND WHAT IS NOT. The signature check lives in
            // `parse::effect_attr` and covers an effect **attribute**:
            // `onTap={navigate("Chat")}` is resolved through its `ImportDecl`
            // to a namespace and an exported name, and its arguments checked
            // against the `FuncSig` the host grants. Nothing checks a
            // `HandlerDecl` body - no pass walks `Stmt`/`Expr::Call` and looks
            // the callee up in a signature list - so the `navigate(LitStr(..))`
            // below agrees with this one only because the test named at the end
            // of this comment asserts it. Every libtsx and libhbui test stayed
            // green with the two disagreeing.
            //
            // THAT IS NOT A GAP TO BE FILLED. Under Rule 46a the authoring
            // surface has no handlers at all - an effect is one call expression
            // in an `on[A-Z]*` attribute - so `HandlerDecl` is a codec shape
            // and THIS FIXTURE IS ITS ONLY WRITER anywhere. A handler-body
            // checker would have nothing to check, and building one would be
            // building for a model this project does not have.
            //
            // So the consistency of the hand-written pair below is a FIXTURE
            // property, asserted by
            // `the_samples_hand_written_handler_agrees_with_the_signatures_beside_it`
            // and by nothing in the library.
            imports: vec![FuncSig {
                name: "navigate".into(),
                params: vec![FieldDecl {
                    name: "to".into(),
                    ty: TypeShape::String,
                    optional: false,
                }],
                result: None,
            }],
            handlers: vec![HandlerDecl {
                sig: FuncSig {
                    name: "onIncrement".into(),
                    params: vec![FieldDecl {
                        name: "step".into(),
                        ty: TypeShape::S32,
                        optional: false,
                    }],
                    result: Some(TypeShape::S32),
                },
                body: vec![
                    Stmt::Expr(Expr::Call {
                        callee: "navigate".into(),
                        args: vec![Expr::LitStr("Chat".into())],
                    }),
                    Stmt::Return(Some(Expr::Bin {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Param(0)),
                        rhs: Box::new(Expr::LitS32(1)),
                    })),
                ],
            }],
        }
    }

    #[test]
    fn dag_round_trips_through_serde() {
        let node = DagNode::Module(sample_module());
        let json = serde_json::to_string(&node).expect("serialize");
        let back: DagNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
    }

    /// **A FIXTURE CHECK, and only that.** It walks the one hand-written
    /// [`HandlerDecl`] in libtsx's own [`sample_module`] and asserts its call
    /// agrees with the [`FuncSig`] written beside it.
    ///
    /// **The authoring model has no handlers** (LIBHBUI_PLAN Rule 46a): an
    /// effect is one call expression in an `on[A-Z]*` attribute, nothing parses
    /// a `HandlerDecl`, and `Definition::handlers` staying empty is correct
    /// rather than a gap. So this is **not** a handler-body checker, nor the
    /// seed of one - the sample below is the only `HandlerDecl` in existence,
    /// and this test exists because that makes it the only thing that can
    /// disagree with itself. `sample_module` carried `navigate(target: S32)`
    /// beside a call passing `LitStr("Chat")` with every test in the workspace
    /// green.
    ///
    /// The two "imports" it touches are different things and are used as such:
    /// the callee is a name, and [`DagModule::imports`] is a list of
    /// **signatures**. There is no [`ImportDecl`] here at all - a fixture
    /// module skips the statement that would bind the name.
    #[test]
    fn the_samples_hand_written_handler_agrees_with_the_signatures_beside_it() {
        let module = sample_module();
        let mut checked = 0;
        for handler in &module.handlers {
            for stmt in &handler.body {
                let Stmt::Expr(Expr::Call { callee, args }) = stmt else {
                    continue;
                };
                let sig = module
                    .imports
                    .iter()
                    .find(|s| &s.name == callee)
                    .unwrap_or_else(|| {
                        panic!("`{callee}` is called and no host signature declares it")
                    });
                assert_eq!(
                    args.len(),
                    sig.params.len(),
                    "`{callee}` is called with {} arguments and declares {}",
                    args.len(),
                    sig.params.len(),
                );
                for (arg, param) in args.iter().zip(&sig.params) {
                    let holds = matches!(
                        (arg, &param.ty),
                        (Expr::LitStr(_), TypeShape::String)
                            | (Expr::LitBool(_), TypeShape::Bool)
                            | (Expr::LitS32(_), TypeShape::S32)
                            | (Expr::LitS64(_), TypeShape::S64)
                            | (Expr::LitF32(_), TypeShape::F32)
                            | (Expr::LitF64(_), TypeShape::F64)
                    );
                    assert!(
                        holds,
                        "`{callee}` passes {arg:?} where `{}` is declared {:?}",
                        param.name, param.ty,
                    );
                }
                checked += 1;
            }
        }
        // Not a count reaching a path - a guard against the walk finding
        // nothing and the test passing by walking an empty body.
        assert_eq!(checked, 1, "the sample declares exactly one host call");
    }

    #[test]
    fn import_decl_round_trips_through_serde() {
        let imp = ImportDecl {
            source: "Library Feed".into(),
            names: vec![
                ImportName { local: "libraryFeed".into(), imported: "libraryFeed".into(), kind: ImportKind::Named },
                ImportName { local: "Feed".into(), imported: "default".into(), kind: ImportKind::Default },
            ],
        };
        let json = serde_json::to_string(&imp).expect("serialize");
        let back: ImportDecl = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(imp, back);
    }

    /// The element tree is graph data, so it serializes like the rest of the
    /// graph: tag, type arguments, attributes and children all survive, in
    /// order. Every `AttrValue` variant and every `Node` variant appears here
    /// so a variant added without a serde derive fails loudly.
    #[test]
    fn element_tree_round_trips_through_serde() {
        let tree = Element {
            tag: "List".into(),
            type_args: vec![TypeShape::Named("Message".into()), TypeShape::String],
            attrs: vec![
                ("value".into(), AttrValue::Binding("chatFeed".into())),
                ("window".into(), AttrValue::Num(24.0)),
                ("title".into(), AttrValue::Str("Chat".into())),
                ("loading".into(), AttrValue::Bool(true)),
                ("style".into(), AttrValue::Opaque),
                (
                    "onTap".into(),
                    AttrValue::NamedEffect(NamedEffect {
                        name: "navigate".into(),
                        args: vec![Expr::LitStr("Chat".into())],
                    }),
                ),
            ],
            children: vec![
                Node::Element(Element {
                    tag: "Item".into(),
                    type_args: vec![],
                    attrs: vec![],
                    children: vec![Node::Text("{{sender}}".into())],
                }),
                Node::Text("plain".into()),
                Node::Expr("user.email".into()),
            ],
        };
        let json = serde_json::to_string(&tree).expect("serialize");
        let back: Element = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tree, back);

        // And as a whole document, alongside its import edges.
        let doc = TsxDocument {
            root_nodes: vec![Node::Element(tree)],
            imports: vec![ImportDecl {
                source: "Chat Feed".into(),
                names: vec![ImportName {
                    local: "chatFeed".into(),
                    imported: "chatFeed".into(),
                    kind: ImportKind::Named,
                }],
            }],
        };
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: TsxDocument = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(doc, back);
    }

    // --- the definition (LIBHBUI_PLAN Rules 9, 17, 18, 21, 41) --------------

    fn a_document() -> TsxDocument {
        TsxDocument {
            root_nodes: vec![Node::Element(Element {
                tag: "Widget".into(),
                type_args: vec![],
                attrs: vec![("props".into(), AttrValue::Str("UserCardProps".into()))],
                children: vec![Node::Element(Element {
                    tag: "Content".into(),
                    type_args: vec![],
                    attrs: vec![],
                    children: vec![Node::Text("{{name}}".into())],
                })],
            })],
            imports: vec![ImportDecl {
                source: "People".into(),
                names: vec![ImportName {
                    local: "people".into(),
                    imported: "people".into(),
                    kind: ImportKind::Named,
                }],
            }],
        }
    }

    fn user_card_props() -> Vec<InterfaceDecl> {
        vec![InterfaceDecl {
            name: "UserCardProps".into(),
            fields: vec![FieldDecl {
                name: "name".into(),
                ty: TypeShape::String,
                optional: false,
            }],
        }]
    }

    #[test]
    fn a_definition_is_one_element_plus_the_nodes_alongside_it() {
        let def = Definition::from_document("UserCard", a_document(), user_card_props())
            .expect("one root element");
        assert_eq!(def.symbol, "UserCard");
        assert_eq!(def.ui.tag, "Widget");
        assert_eq!(def.ui.children.len(), 1);
        assert_eq!(def.imports.len(), 1, "the import edge came along");
        assert_eq!(def.interfaces.len(), 1, "so did the interface");
        assert!(def.handlers.is_empty());
    }

    #[test]
    fn a_source_with_two_roots_is_not_a_definition() {
        // The type cannot hold two elements; this is the SOURCE being refused,
        // and refused rather than silently truncated to its first root.
        let mut doc = a_document();
        doc.root_nodes.push(Node::Element(Element {
            tag: "Stowaway".into(),
            type_args: vec![],
            attrs: vec![],
            children: vec![],
        }));
        assert_eq!(
            Definition::from_document("UserCard", doc, vec![]),
            Err(DefError::SeveralRoots(2))
        );

        let empty = TsxDocument { root_nodes: vec![], imports: vec![] };
        assert_eq!(
            Definition::from_document("UserCard", empty, vec![]),
            Err(DefError::NoUi)
        );

        let texty = TsxDocument {
            root_nodes: vec![Node::Text("just words".into())],
            imports: vec![],
        };
        assert_eq!(
            Definition::from_document("UserCard", texty, vec![]),
            Err(DefError::RootNotAnElement)
        );
    }

    #[test]
    fn the_container_names_the_props_shape() {
        let def = Definition::from_document("UserCard", a_document(), user_card_props())
            .expect("definition");
        assert_eq!(def.props_name(), Ok(Some("UserCardProps")));
        assert_eq!(
            def.props_shape().expect("resolves").map(|i| i.name.as_str()),
            Some("UserCardProps")
        );

        // `props={Shape}` reads the same as `props="Shape"`.
        let mut binding = def.clone();
        binding.ui.attrs = vec![("props".into(), AttrValue::Binding("UserCardProps".into()))];
        assert_eq!(binding.props_name(), Ok(Some("UserCardProps")));

        // Undeclared is None; declared-but-not-a-name is an error, not a None
        // (Rule 10); declared-but-unresolvable is an error too (Rule 30).
        let mut none = def.clone();
        none.ui.attrs.clear();
        assert_eq!(none.props_name(), Ok(None));
        assert_eq!(none.props_shape().map(|o| o.is_none()), Ok(true));

        let mut wrong = def.clone();
        wrong.ui.attrs = vec![("props".into(), AttrValue::Num(3.0))];
        assert_eq!(
            wrong.props_name(),
            Err(DefError::ReservedAttrNotAName("props"))
        );

        let mut dangling = def.clone();
        dangling.interfaces.clear();
        assert_eq!(
            dangling.props_shape(),
            Err(DefError::UnknownPropsShape("UserCardProps".into()))
        );
    }

    #[test]
    fn only_the_outer_container_is_read_for_reserved_attributes() {
        // A CHILD carrying `props` is data, not a declaration: reading it here
        // would be exactly the special prop handling Rule 41 confines to the
        // container.
        let mut def = Definition::from_document("UserCard", a_document(), user_card_props())
            .expect("definition");
        def.ui.attrs.clear();
        let Node::Element(child) = &mut def.ui.children[0] else {
            panic!("the child is an element")
        };
        child.attrs.push(("props".into(), AttrValue::Str("Sneaky".into())));
        assert_eq!(def.props_name(), Ok(None));
    }

    #[test]
    fn a_definition_round_trips_through_serde() {
        let def = Definition::from_document("UserCard", a_document(), user_card_props())
            .expect("definition");
        let json = serde_json::to_string(&def).expect("serialize");
        let back: Definition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(def, back);
    }

    // --- effect bindings (LIBHBUI_PLAN Rules 46a, 48) -----------------------

    /// **The recognition rule, and nothing beside it** (Rule 46a). `on`
    /// followed by an upper-case letter, mechanically - so the near misses
    /// matter more than the hits: `on`, `once` and `only` all begin with `on`
    /// and none of them announces an event, and a list-based rule would have
    /// had to remember each one.
    #[test]
    fn an_event_binding_announces_itself_by_its_name() {
        for yes in ["onTap", "onLongPress", "onX", "onSubmit"] {
            assert!(is_event_binding(yes), "`{yes}` is an event binding");
        }
        for no in [
            "on",       // nothing follows
            "once",     // lower case follows
            "only",     //
            "on1",      // a digit is not an upper-case letter
            "on_Tap",   //
            "column",   // contains "on", does not start with it
            "direction",//
            "ontap",    // the case IS the rule
            "",         //
            "Ontap",    //
        ] {
            assert!(!is_event_binding(no), "`{no}` is not an event binding");
        }
    }

    /// A grant is what the host offers; a namespace nothing grants declares
    /// nothing, and a granted one declares only what it was given.
    #[test]
    fn a_host_grant_answers_only_for_what_it_granted() {
        let navigate = FuncSig {
            name: "navigate".into(),
            params: vec![FieldDecl {
                name: "to".into(),
                ty: TypeShape::String,
                optional: false,
            }],
            result: None,
        };
        let host = HostEffects::granting("host:effects", vec![navigate.clone()]);
        assert_eq!(host.declares("host:effects", "navigate"), Some(&navigate));
        assert_eq!(host.declares("host:effects", "teleport"), None);
        assert_eq!(host.declares("host:other", "navigate"), None);
        assert_eq!(host.namespace("host:other"), None);
        assert_eq!(HostEffects::none().declares("host:effects", "navigate"), None);

        // The specifier form is what tells a host namespace from a Script's
        // display name or a package path (Rule 48).
        assert!(is_host_namespace("host:effects"));
        assert!(!is_host_namespace("Library Feed"));
        assert!(!is_host_namespace("./widgets/UserCard"));
        assert!(!is_host_namespace("@highbay/effects"));
    }

    /// A grant no import could ever name is a mistake in the embedding, and a
    /// silent one is indistinguishable from an effect that does not resolve.
    #[test]
    #[should_panic(expected = "is granted as a host namespace")]
    fn granting_a_namespace_that_is_not_one_is_a_mistake() {
        HostEffects::granting("Library Feed", vec![]);
    }

    #[test]
    fn optional_fields_default_when_absent() {
        // Forward-compat: older serializations without `optional`/`result`
        // still deserialize.
        let json = r#"{ "name": "x", "ty": "Bool" }"#;
        let field: FieldDecl = serde_json::from_str(json).expect("deserialize");
        assert!(!field.optional);
        let json = r#"{ "name": "f", "params": [] }"#;
        let sig: FuncSig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(sig.result, None);
    }
}
