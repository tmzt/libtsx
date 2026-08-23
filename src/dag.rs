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
//! * **Imported calls** ([`ImportedCall`], [`AttrValue::ImportedCall`]) -
//!   `onGrommet={frobnicate("sprocket")}`: an [`is_event_binding`] attribute whose value
//!   is ONE call resolving to a granted host import. Deliberately *not* a
//!   handler and not a body - see [`ImportedCall`] and [`HOST_PREFIX`]
//!   (LIBHBUI_PLAN Rules 46, 46a, 48).
//! * **The embedding's provider** ([`ParserHost`], [`Resolution`]) - what a
//!   module specifier resolves to. Not graph data: it is the question the
//!   parse asks whoever embeds it, and the reason no name from any embedding's
//!   model appears in this crate (Rule 52).
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

use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
    /// This is *what a name may be called as*. An imported call needs BOTH:
    /// the declaration binds the local name, this types the call. Resolving a
    /// call against the wrong one is a defect, not a shortcut - see
    /// `parse::ImportScope`, which walks the declaration to a namespace and an
    /// exported name and only then asks the grant for the signature.
    pub imports: Vec<FuncSig>,
    /// Event handlers.
    ///
    /// **Empty in everything the authoring surface produces, and that is
    /// correct rather than a gap** (LIBHBUI_PLAN Rule 46a): what an `on[A-Z]*`
    /// attribute carries is one call expression, so there is no handler to
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
/// signature: what the name may be *called as* lives in a [`FuncSig`] - behind
/// [`Resolution::Host`] for a granted host namespace, in
/// [`DagModule::imports`] once a module is assembled. The two are chained,
/// never interchangeable: a local name resolves through this declaration to a
/// namespace and an exported name, and only then to the signature that types
/// the call.
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
    /// `onGrommet={frobnicate("sprocket")}` - a **call to an imported symbol**
    /// (LIBHBUI_PLAN Rules 46, 46a, 48).
    ///
    /// The names in that spelling are libtsx's own placeholders and belong to
    /// no embedding: what an attribute or an imported name MEANS is the
    /// embedding's model (Rule 52, [`ParserHost`]).
    ///
    /// The attribute name matched [`is_event_binding`], so the author declared
    /// an event binding; the value was one call expression resolving to a
    /// declared host import. See [`ImportedCall`].
    ///
    /// # It was called `NamedEffect`, and that name claimed a meaning
    ///
    /// An "effect" is something that HAPPENS - it is run, it mutates, it
    /// navigates. None of that is knowable here. libtsx read a call, resolved
    /// its callee through the import chain, and checked its arguments against
    /// the signature the embedding handed over; whether the thing on the other
    /// end is an effect, a pure query or a no-op is the embedding's model, and
    /// naming this variant after one of those readings put the embedding's
    /// vocabulary into the parser's (Rule 52, the same rule
    /// [`ParserHost`] exists to keep).
    ///
    /// **Kept after all preceding variants on purpose.** postcard encodes enum
    /// variants positionally, so existing values above it must not move - and
    /// the rename costs no bytes for the same reason: postcard writes indices,
    /// never names.
    ImportedCall(ImportedCall),
    /// An owned object/data binding expression. This is deliberately separate
    /// from [`Binding`] and [`ImportedCall`]: the former is the legacy path
    /// spelling and the latter is the narrow, checked event grammar.
    ///
    /// **Appended last on purpose.** `AttrValue` is persisted positionally.
    BindingExpr(BindingExpr),
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

/// **A call to an imported symbol**: a declared host import, named, with its
/// arguments (LIBHBUI_PLAN Rules 46a, 48).
///
/// This is all libtsx knows and all it is entitled to know. The parse resolved
/// [`name`] through the module's import chain to an exported name in
/// [`namespace`], and checked [`args`] against the [`FuncSig`] that namespace
/// declares. What the call DOES on the other side is the embedding's model.
///
/// # It was called `NamedEffect`, and the name misled two ways
///
/// **"Effect" was a claim libtsx cannot make.** The word says the callee runs
/// something - navigates, mutates, fires. libtsx never learns that: it sees an
/// import, a callee, a signature and some literals. Naming the type after the
/// embedding's reading of the callee is the same mistake [`ParserHost`] is
/// named to avoid (Rule 52), and it has since become actively ambiguous -
/// "effect" now also names a pipeline stage (`libeffects`) and a value
/// accessor, neither of which is this.
///
/// **"Named" was defending the wrong thing.** It was there to say a call here
/// is never anonymous, but that is a property of being a resolved IMPORT: a
/// carrier holding an arbitrary [`Expr`] could not be an imported call at all,
/// because there would be nothing to resolve. The new name carries that
/// guarantee in the noun, so the adjective has no work left to do.
///
/// **The identity is the QUALIFIED name** (Rule 48): [`namespace`] and [`name`]
/// together, never the bare name. A bare name is only unique inside one grant,
/// and there is more than one grant - a provider answers for as many namespaces
/// as the embedding declares. Dropping the namespace here would make two
/// namespaces exporting a `frobnicate` with different meanings the same call
/// to every reader downstream, and a decoded tree carrying one of them would
/// pass a grant check written against the other.
///
/// [`name`]: ImportedCall::name
/// [`namespace`]: ImportedCall::namespace
/// [`args`]: ImportedCall::args
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedCall {
    /// The **specifier it was imported from** - the granted host namespace, as
    /// the source spelled it and as the provider answered for it. Half the
    /// identity, and the half a bare name cannot recover.
    pub namespace: String,
    /// The host import's **exported** name - the `imported` half of the import
    /// chain, so `import { frobnicate as fb }` and a call to `fb(..)` both
    /// arrive here as `frobnicate`. An alias is resolved once, at parse, rather
    /// than at every reader.
    pub name: String,
    /// The call's arguments, in source order, lowered against the declared
    /// parameter types.
    ///
    /// **Literals and binding paths**, and nothing else - this call grammar is
    /// not an expression language (Rule 46a). A literal arrives as the
    /// `Expr::Lit*` its declared parameter type chose; a binding path (`{id}`,
    /// `{props.user.name}`) arrives as [`Expr::Get`], which is the existing
    /// path-read variant and not a new one, so a decoder that never met a
    /// binding argument still knows the shape it arrives in. Everything that
    /// computes is [`EffectError::ArgNotALiteral`].
    pub args: Vec<Expr>,
}

/// A literal accepted by an owned object/data binding expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BindingLiteral {
    Bool(bool),
    Number(f64),
    String(String),
    Null,
}

/// The owned expression vocabulary for object-valued bindings.
///
/// This is intentionally independent of [`Expr`] and of the legacy event
/// grammar. In particular, adding this vocabulary does not make an
/// `AttrValue::Opaque` event expression executable.
///
/// # This vocabulary captures TS-LEVEL SYNTAX, not meaning
///
/// A variant here is justified by TypeScript having the form, and by nothing
/// else - never by a consumer wanting a behaviour. `??` and `? :` are captured
/// because an author can write them; what they MEAN (a null test, a branch
/// selection) is supplied by a further lowering, which for an attribute is
/// `libhbui::attr`. Two consumers may lower one variant differently and neither
/// is wrong here; a variant added *for* one consumer's semantics would make
/// this enum that consumer's private IR.
///
/// # APPEND-LAST, always
///
/// `BindingExpr` reaches disk inside [`AttrValue::BindingExpr`], and `AttrValue`
/// is persisted POSITIONALLY by postcard - the committed `.hbdef` fixtures carry
/// bare variant indices with no names in the bytes. So a new variant goes at the
/// END, after every variant that already exists, or every committed fixture
/// decodes as a different expression. Appending keeps old bytes readable; old
/// readers refuse new bytes, which is the version-gate event, and it starts the
/// day an author WRITES one of the new forms - not the day the variant lands.
///
/// # A REMOVAL cannot be append-managed, and there has been one
///
/// The discipline above governs WITHIN a schema version and has nothing to
/// offer a variant that LEAVES: every variant after the gap shifts down one
/// index, so bytes written before the removal decode as a different expression
/// with no error anywhere. `Map { source, param, body }` left here -
/// `PIPELINE_PLAN.md` section 6b, on the ground that reading a callee named
/// `map` as a comprehension is a MEANING and this enum captures forms - and
/// `xs.map(x => x)` is now a [`Call`](BindingExpr::Call) whose argument is an
/// [`Arrow`](BindingExpr::Arrow), which is what TypeScript says it is. That
/// removal is why `libhbui`'s `HBDEF_VERSION` is 2. A future removal is another
/// such event and costs another bump; there is no cheaper way to take one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BindingExpr {
    Literal(BindingLiteral),
    Path(Vec<String>),
    Array(Vec<BindingExpr>),
    Record(Vec<(String, BindingExpr)>),
    Call {
        namespace: String,
        name: String,
        type_args: Vec<TypeShape>,
        args: Vec<BindingExpr>,
    },
    Async(BlockArrow),
    /// `a ?? b`, and `a ?? b ?? c` as ONE n-ary node.
    ///
    /// **Appended after [`Async`](BindingExpr::Async) on purpose** - see the
    /// append-last rule on this enum.
    ///
    /// N-ary rather than a nested pair because `??` is left-associative and
    /// non-mixing in TS, so `a ?? b ?? c` has exactly one reading and flattens
    /// without losing anything a re-parse could tell apart. A nested spelling
    /// would give one source two representations and hand every consumer the
    /// job of normalising them.
    ///
    /// The vector holds the operands in source order and always has at least
    /// two entries; the parser builds no shorter one. `||` and `&&` are NOT
    /// this variant and stay refused - TS forbids them mixing with `??`
    /// unparenthesised precisely because they answer a different question
    /// (falsy vs nullish), and collapsing the two spellings here would decide
    /// that question in the wrong layer.
    Coalesce(Vec<BindingExpr>),
    /// `cond ? then : other` - the conditional (ternary) expression.
    ///
    /// **Appended after [`Coalesce`](BindingExpr::Coalesce) on purpose.**
    ///
    /// Boxed on all three arms because the shape is recursive and the branches
    /// are ordinary expressions, including further conditionals. What makes
    /// `cond` true is not decided here: this variant records that the author
    /// wrote a branch, and the lowering says what a condition value means.
    Cond {
        cond: Box<BindingExpr>,
        then: Box<BindingExpr>,
        other: Box<BindingExpr>,
    },
    /// A static member chain on a NON-identifier base: `f().x.y`.
    ///
    /// **Appended after [`Cond`](BindingExpr::Cond) on purpose, and it is the
    /// genuinely new capture of the three.**
    ///
    /// [`Path`](BindingExpr::Path) roots at an identifier, so `props.value` is
    /// a path and `design().isAuthoring` is not representable by it at all -
    /// the base is a CALL. The alternative considered and rejected was folding
    /// the member chain into the call (as extra arguments, or by appending to
    /// its name): that avoids a variant by writing down something the author
    /// did not write, and it stops being reversible the moment two accessors
    /// disagree about what a suffix means. Capturing the member access as a
    /// member access is what the capture-the-syntax rule asks for, and it
    /// subsumes every later accessor without a further variant.
    ///
    /// `base` is the expression the chain hangs off; `path` is the
    /// dot-separated static segments after it, in source order, never empty.
    /// Computed access (`a[b]`) is not this variant and stays refused.
    Member {
        base: Box<BindingExpr>,
        path: Vec<String>,
    },
    /// `a === b` and `a == b` - an equality test.
    ///
    /// **Appended after [`Member`](BindingExpr::Member) on purpose.**
    ///
    /// # Why `strict` is recorded rather than normalised away
    ///
    /// TypeScript has two equality operators and they are not the same
    /// question: `===` compares without conversion, `==` converts first. A
    /// capture that folded them together would be this layer answering "does
    /// coercion happen?" - which is a MEANING, and meanings belong to the
    /// lowering (see the enum's own doc). So the operator the author wrote is
    /// recorded, the emitter puts back what it was given, and each consumer
    /// decides for itself.
    ///
    /// **What the first consumer decided, recorded here because it is
    /// surprising**: `libhbui::attr` evaluates an equality only when both sides
    /// resolve to the SAME scalar kind, and refuses a cross-kind comparison
    /// loudly. Under that rule `strict` never changes an answer - the two
    /// operators differ exactly where coercion would have to happen, and that
    /// case is refused rather than decided. The field is carried for fidelity,
    /// not for behaviour, and a consumer that later wants JS coercion
    /// semantics has the fact it needs instead of having to guess.
    ///
    /// `!=`/`!==` are NOT this variant and stay refused: a negation is a second
    /// operator, nothing has asked for it, and inferring it from an equality
    /// would be inventing syntax the author did not write.
    Eq {
        left: Box<BindingExpr>,
        right: Box<BindingExpr>,
        /// `true` for `===`, `false` for `==`.
        strict: bool,
    },
    /// `x => x` - an arrow function with an EXPRESSION body.
    ///
    /// **Appended after [`Eq`](BindingExpr::Eq) on purpose.**
    ///
    /// # Why it is here, and what it replaced
    ///
    /// TypeScript has no comprehension expression: `xs.map(x => x)` is a
    /// method call whose argument is an arrow. This enum used to hold a `Map
    /// { source, param, body }` variant instead, which read that call as a
    /// comprehension - a JUDGMENT about what a callee named `map` means, and
    /// therefore a meaning rather than a form (see the enum doc). With `Map`
    /// gone the call is captured as a [`Call`](BindingExpr::Call) whose
    /// argument is this variant, which is what the author wrote; the
    /// comprehension reading belongs to whichever consumer wants it.
    ///
    /// # The two arrow spellings, and the subset boundary between them
    ///
    /// The BLOCK-bodied arrow is [`Async`](BindingExpr::Async), whose payload
    /// is an [`BlockArrow`] - `params` plus a declared subset of a TS block.
    /// This variant is the other half: an expression body, no block, and no
    /// `async`. The two combinations neither covers - a non-async block body,
    /// and an `async` expression body - are refused AT CAPTURE, which is a
    /// decision about which TypeScript the DAG accepts and not a meaning
    /// layered onto it. Growing the accepted set is the ordinary
    /// capture-the-language cost, paid when an author needs to write the form.
    ///
    /// `params` reuses [`BindingParam`] so both arrow spellings describe their
    /// parameters identically; an unannotated parameter arrives with
    /// `TypeShape::Named("unknown")`, exactly as it does for
    /// [`BlockArrow`]. A destructuring or rest parameter is refused.
    ///
    /// **The body is not primary and the emitter must not splice it bare.** An
    /// arrow body runs to the end of the expression, so `x => x` beside any
    /// operator has to be parenthesised, and a body that is a RECORD has to be
    /// parenthesised the other way round (`x => ({a: 1})`), or the `{` opens a
    /// block statement and the re-parse means something else.
    Arrow {
        params: Vec<BindingParam>,
        body: Box<BindingExpr>,
    },
}

/// **A top-level identifier an expression CALLS as a value** - the `it` of
/// `it()`, the `design` of `design().isAuthoring`.
///
/// # Why an identifier registry belongs in THIS vocabulary
///
/// [`BindingExpr`]'s rule is that a variant is justified by TypeScript having
/// the FORM and by nothing else, never by a consumer wanting a behaviour. This
/// type exists to be a parameter of that enum, so it answers to the same rule.
/// The form it names is **"a call to an identifier, optionally with a member on
/// it"** - which TypeScript has for every identifier there is. Nothing about
/// that shape is a reading. What an identifier MEANS - which scope answers
/// `it()`, whether `design()` varies by surface - stays with the lowering that
/// asks, `libhbui::attr`, exactly as the rule requires.
///
/// The contrast is `Map { source, param, body }`, removed from [`BindingExpr`]
/// at the cost of a version bump (see its doc): `Map` RESTRUCTURED a call into
/// three named parts, a shape TypeScript does not have and only a comprehension
/// READING produces. A symbol variant restructures nothing. A symbol value
/// spelled `It` and one spelled `Named("it")` denote the same expression, and
/// neither of them says what `it` means.
///
/// # The set is OPEN, and [`Named`](ObjectSymbol::Named) is what says so
///
/// The model is HTTP HEADERS. A header's wire value IS its name: `Content-Type`
/// travels as those characters, well-known names get interned constants, anyone
/// may send `X-Anything`, and the registry grows without any message becoming
/// unreadable - and nobody reads a library's `CONTENT_TYPE` constant as that
/// library adopting a caller's semantics. The variants below are exactly that:
/// a REGISTRY of the spellings Highbay writes, a compression of a string rather
/// than an interpretation of one. Which identifiers are worth interning is a
/// convenience judgement, not a semantic one, and another embedding spells its
/// own through `Named` and loses nothing.
///
/// **That is what this prevents**: an enum without `Named` would make adding a
/// variant this crate deciding which identifiers may EXIST - a claim about
/// meaning wearing a claim about form, which is the one thing the vocabulary
/// rule is here to keep out.
///
/// # The encoding is the IDENTIFIER, decided rather than defaulted
///
/// `Serialize`/`Deserialize` are hand-written, and the header analogy is the
/// reason: the bytes are `"it"` whether the value is [`It`](ObjectSymbol::It)
/// or `Named("it")`, so PROMOTING an identifier into the interned set changes
/// no committed byte. A derive would encode POSITIONALLY under postcard - which
/// is how `AttrValue` reaches disk - and then one promotion would make
/// committed `.hbdef` bytes and a fresh parse two different values for one
/// expression, costing a fixture regeneration every time the registry grows. A
/// derive is what you get by NOT deciding; this is the decision, and it is
/// pinned by `a_symbol_encodes_as_its_identifier_whether_or_not_it_is_interned`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectSymbol {
    /// `it()` - the enclosing item.
    It,
    /// `listItem()` - the iterator's current position, as against its fields.
    ListItem,
    /// `design()` - the surface being drawn on.
    Design,
    /// `uiSite()` - what this instance is DOING: interaction state owned at the
    /// site.
    UiSite,
    /// `uiComponent()` - the call site that instanced this body, and so what
    /// the caller handed it.
    UiComponent,
    /// Any other identifier, spelled as written.
    ///
    /// Not a refusal and not an error. An identifier with no interned variant
    /// is carried here and means whatever the consumer asking makes of it; a
    /// recognizer therefore never has to decide that an unknown callee is
    /// wrong, which is a judgement only the consumer holding a scope can make.
    Named(String),
}

impl ObjectSymbol {
    /// The identifier [`It`](ObjectSymbol::It) is spelled with.
    pub const IT: &'static str = "it";
    /// The identifier [`ListItem`](ObjectSymbol::ListItem) is spelled with.
    pub const LIST_ITEM: &'static str = "listItem";
    /// The identifier [`Design`](ObjectSymbol::Design) is spelled with.
    pub const DESIGN: &'static str = "design";
    /// The identifier [`UiSite`](ObjectSymbol::UiSite) is spelled with.
    pub const UI_SITE: &'static str = "uiSite";
    /// The identifier [`UiComponent`](ObjectSymbol::UiComponent) is spelled
    /// with.
    pub const UI_COMPONENT: &'static str = "uiComponent";

    /// **The identifier this symbol IS** - the authored spelling and the wire
    /// value, which are one string (see the type doc).
    pub fn as_str(&self) -> &str {
        match self {
            Self::It => Self::IT,
            Self::ListItem => Self::LIST_ITEM,
            Self::Design => Self::DESIGN,
            Self::UiSite => Self::UI_SITE,
            Self::UiComponent => Self::UI_COMPONENT,
            Self::Named(name) => name.as_str(),
        }
    }

    /// The symbol `name` spells - interned when it is a registered spelling,
    /// [`Named`](ObjectSymbol::Named) when it is not.
    ///
    /// **Total by construction, and that is the point.** There is no identifier
    /// this refuses, so interning can never turn an authored expression into a
    /// parse error, and a later promotion can never change which expressions
    /// are accepted - only which variant carries one.
    pub fn intern(name: &str) -> Self {
        match name {
            Self::IT => Self::It,
            Self::LIST_ITEM => Self::ListItem,
            Self::DESIGN => Self::Design,
            Self::UI_SITE => Self::UiSite,
            Self::UI_COMPONENT => Self::UiComponent,
            other => Self::Named(other.to_string()),
        }
    }
}

/// **One member hop off an expression** - the `isAuthoring` of
/// `design().isAuthoring`, the `props` and the `textColor` of
/// `uiComponent().props.textColor`.
///
/// The same shape as [`ObjectSymbol`] and there for the same reasons: it names
/// a member ACCESS, which TypeScript has for any identifier, and the interned
/// variants are a registry of the spellings Highbay writes rather than a claim
/// about which members exist. Read that type's doc for the vocabulary rule, the
/// HTTP-header model, and why the encoding is the identifier.
///
/// **`Named` is load-bearing HERE for a stronger reason than there.** A
/// record's fields are arbitrary - they come from whatever interface an author
/// declared - so no enum could ever enumerate them, and a closed one would make
/// `it().customerRef` unrepresentable. The interned set is exactly the members
/// a reader in this workspace already spells as a constant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PropertyAccessor {
    /// `.value` - what an object carries, as against what it IS.
    Value,
    /// `.id` - an object's identity.
    Id,
    /// `.index` - a position within an iteration.
    Index,
    /// `.props` - the namespace a call site's arguments hang under, as in
    /// `uiComponent().props.NAME`.
    Props,
    /// `.isAuthoring` - whether the surface is being authored rather than run.
    IsAuthoring,
    /// Any other member, spelled as written - a record's own fields, which are
    /// arbitrary by definition.
    Named(String),
}

impl PropertyAccessor {
    /// The identifier [`Value`](PropertyAccessor::Value) is spelled with.
    pub const VALUE: &'static str = "value";
    /// The identifier [`Id`](PropertyAccessor::Id) is spelled with.
    pub const ID: &'static str = "id";
    /// The identifier [`Index`](PropertyAccessor::Index) is spelled with.
    pub const INDEX: &'static str = "index";
    /// The identifier [`Props`](PropertyAccessor::Props) is spelled with.
    pub const PROPS: &'static str = "props";
    /// The identifier [`IsAuthoring`](PropertyAccessor::IsAuthoring) is spelled
    /// with.
    pub const IS_AUTHORING: &'static str = "isAuthoring";

    /// **The identifier this accessor IS** - see [`ObjectSymbol::as_str`].
    pub fn as_str(&self) -> &str {
        match self {
            Self::Value => Self::VALUE,
            Self::Id => Self::ID,
            Self::Index => Self::INDEX,
            Self::Props => Self::PROPS,
            Self::IsAuthoring => Self::IS_AUTHORING,
            Self::Named(name) => name.as_str(),
        }
    }

    /// The accessor `name` spells - see [`ObjectSymbol::intern`], which this is
    /// the member-side twin of.
    pub fn intern(name: &str) -> Self {
        match name {
            Self::VALUE => Self::Value,
            Self::ID => Self::Id,
            Self::INDEX => Self::Index,
            Self::PROPS => Self::Props,
            Self::IS_AUTHORING => Self::IsAuthoring,
            other => Self::Named(other.to_string()),
        }
    }
}

impl Serialize for ObjectSymbol {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ObjectSymbol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::intern(&String::deserialize(deserializer)?))
    }
}

impl Serialize for PropertyAccessor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PropertyAccessor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::intern(&String::deserialize(deserializer)?))
    }
}

/// A named parameter of either arrow spelling - [`BlockArrow`]'s and
/// [`BindingExpr::Arrow`]'s alike.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingParam {
    pub name: String,
    pub ty: TypeShape,
}

/// **An arrow function with a BLOCK body**, whose block is a declared SUBSET of
/// a TypeScript block: `params`, and the statement forms [`BlockStmt`] admits.
///
/// # It used to be called `BlockArrow`, and the old name was wrong about it
///
/// The doc here read *"A closed, owned effect program. It is semantic IR, never
/// JavaScript"*, and `PIPELINE_PLAN.md` section 6b measured that sentence
/// against the type it described. Each of the five statement forms is
/// TS-shaped, so the body is a subset of a TS block - and a subset of TS is
/// still TS. What was never here was the EFFECT: dormancy, being run by a
/// runtime, an await that suspends. Every one of those is a reading a consumer
/// applies to this shape, not a property the shape has, and calling the type
/// after the reading put a consumer's semantics into a vocabulary whose whole
/// rule (see [`BindingExpr`]) is that it captures forms and no meanings.
///
/// So: renamed and re-documented in place, not relocated. It is dag vocabulary,
/// it stays with the dag, and the rename costs no bytes - postcard encodes
/// positions, never names.
///
/// **This is one half of the arrow capture**; [`BindingExpr::Arrow`] is the
/// other, and the boundary between them is written down there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockArrow {
    pub params: Vec<BindingParam>,
    pub body: Vec<BlockStmt>,
}

/// The statement forms a [`BlockArrow`]'s block admits, and the only ones.
///
/// **A declared subset of TypeScript's statements, not a semantic IR of its
/// own.** `let`/`await`/`if`/`try`/`return` are each TS-shaped; refusing what
/// is OUTSIDE the subset is a decision about which TypeScript the DAG accepts,
/// which capture is allowed to make. Growing the subset is the ordinary
/// capture-the-language cost, paid per form when an author needs to write it.
/// A consumer accepting LESS than the subset is a different thing entirely and
/// stays a named refusal at the reader - `highbay_objects`' plan compiler
/// refuses `If`/`Try` that the parser happily produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BlockStmt {
    Let {
        slot: String,
        value: BindingExpr,
    },
    Await {
        slot: Option<String>,
        awaitable: BindingExpr,
    },
    If {
        condition: BindingExpr,
        then_branch: Vec<BlockStmt>,
        else_branch: Vec<BlockStmt>,
    },
    Try {
        body: Vec<BlockStmt>,
        error_slot: String,
        catch: Vec<BlockStmt>,
    },
    Return(BindingExpr),
}


/// **What an import specifier resolves to** - the one question the parse asks
/// its embedding (LIBHBUI_PLAN Rules 48, 52).
///
/// Rule 48 names three answers and this is all three.
///
/// **The parse's split is not three ways, it is `Host` / `{Script, Package}` /
/// `None`**, and saying so is the honest version. Only [`Resolution::Host`]
/// supplies anything the parse can use - a signature to check a call against.
/// A Script and a package are *compiled* or *fetched* by somebody else, so a
/// call through either is refused identically, as
/// [`EffectError::NotAHostImport`] naming the specifier; and a specifier the
/// provider does not know at all is the third answer,
/// [`EffectError::Unresolved`], which says "you never imported that" rather
/// than "you imported that from something with a source behind it".
///
/// **The two are still distinct here because the EMBEDDING distinguishes
/// them**, not the parse: a Script is a display name in the Highbay module
/// system and a package is a real path, and a provider that had to collapse
/// them would be losing a fact it owns in order to answer a question the parse
/// does not ask. Anything the parse could do differently with a package would
/// arrive as a payload on this variant, as it did for `Host`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resolution<'a> {
    /// A **Script** in the Highbay module system, named by its display name.
    /// Compiled elsewhere; nothing here reads it.
    Script,
    /// A real **package path**. Resolved outside the parse, exactly as it
    /// always has been.
    Package,
    /// A **granted host namespace** and the signatures it declares - an import
    /// with no source, whose qualified name *is* its identity (Rule 48).
    ///
    /// The only answer that supplies anything an [`ImportedCall`] can resolve
    /// against.
    Host(&'a [FuncSig]),
}

/// **The parser's package provider**: what the embedding tells the parse about
/// a module specifier (LIBHBUI_PLAN Rule 52).
///
/// Named for its DEFINER. The parser is the thing that needs a host, and
/// whoever embeds the parser supplies one; a trait here named for its one
/// current implementor would be libtsx naming its client, which is the shape
/// that invites a dependency cycle even where there is not one yet.
///
/// **It is a package provider, not a host-import grant.** The parser's actual
/// question is *what does this specifier resolve to*, and one provider answers
/// all three of Rule 48's answers ([`Resolution`]). Building it host-imports-
/// first would need `set_script_resolver` next and a third setter after that -
/// a family of setters inside the very interface introduced to stop one
/// (Rule 49's defect one level down).
///
/// **libtsx knows the SHAPE, never the NAMES.** This crate knows that an
/// `on[A-Z]*` attribute carries a named call with literal arguments, that a
/// `host:` specifier is spelled with a scheme ([`HOST_PREFIX`]), and that
/// arguments are checked against a [`FuncSig`] it is handed. WHICH attribute
/// names an event, WHAT a given imported name MEANS, and what lives under a
/// given host namespace are the *model*, and the model belongs to the
/// embedding. libtsx's own tests therefore run against a mock provider
/// granting a vocabulary no embedding uses, so a name leaking down here fails
/// a test instead of passing unnoticed.
pub trait ParserHost {
    /// What this specifier resolves to, or `None` for one this provider does
    /// not resolve at all.
    ///
    /// `None` is not "refuse it": a specifier is only refused where the parse
    /// can tell it is wrong, which is when its **scheme** says host and the
    /// provider does not grant it. A non-host specifier the provider does not
    /// know is left alone, because whether it names a real Script has never
    /// been the parser's question.
    fn resolve(&self, specifier: &str) -> Option<Resolution<'_>>;
}

/// Why an effect binding cannot mean what it says (LIBHBUI_PLAN Rules 46a, 48).
///
/// Every variant is a **declaration** that is wrong, and every one of them is a
/// refusal rather than an `AttrValue::Opaque` that silently does nothing. That
/// is the whole reason detection and carriage are one step: an attribute
/// matching [`is_event_binding`] announces itself, so there is no case in which
/// "we could not lower this" and "the author wrote no effect" arrive as the
/// same value (Rule 10).
///
/// **It is the element conversion's refusal type, not only the effect
/// grammar's**, and has been since [`EffectError::SpreadAttribute`] and
/// [`EffectError::BindingSyntax`]: what unites the variants is Rule 10, not the
/// `on..` prefix. [`EffectError::UnreadableChild`] is the same rule one
/// position over - a CHILD nothing can hold is now named here rather than
/// dropped where nobody could see it.
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
    /// The callee names something this module **did** import - and imported
    /// from a Script or a package rather than from a granted host namespace
    /// ([`Resolution`]).
    ///
    /// A Script is compiled and a host import is granted (Rule 48); only the
    /// second has a [`FuncSig`] for the call to be checked against, so this is
    /// a different fact from [`EffectError::Unresolved`] and says so. Without
    /// the provider it could only be reported as "not imported", which is
    /// wrong about the source in the way most likely to waste a reader's time.
    NotAHostImport {
        /// The attribute that announced an effect.
        attr: String,
        /// The callee as written.
        callee: String,
        /// The specifier it was imported from.
        source: String,
    },
    /// The provider resolved a specifier to a granted host namespace
    /// ([`Resolution::Host`]) that is **not spelled as one** ([`HOST_PREFIX`]).
    ///
    /// A mistake in the embedding, not in the source. The scheme is what lets
    /// the parse tell a granted host import from a Script *before* consulting
    /// the provider, so a grant no import could ever be recognised as one is a
    /// capability nothing can reach - and a silent one looks exactly like an
    /// effect that does not resolve.
    GrantedWithoutScheme {
        /// The specifier the provider granted.
        source: String,
    },
    /// The call supplies a number of arguments the signature cannot be
    /// satisfied by: more than it declares, or too few to fill every
    /// **non-optional** parameter.
    ///
    /// **Optionality is modelled** ([`FieldDecl::optional`]): arguments fill
    /// parameters positionally, and a call is refused when any parameter left
    /// unfilled is required. So a signature declaring `(to: string, id?:
    /// string)` accepts one argument or two and refuses zero or three, and
    /// [`required`] is what says which of those it was.
    ///
    /// [`required`]: EffectError::ArgCount::required
    ArgCount {
        /// The attribute that announced an effect.
        attr: String,
        /// The host import's exported name.
        effect: String,
        /// How many parameters the signature declares in total.
        declared: usize,
        /// How many of them are **not** optional - the fewest arguments a call
        /// can supply. Equal to `declared` for a signature with no optional
        /// parameters, which is every signature that existed before optionality
        /// was modelled.
        required: usize,
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
    /// An argument is neither a literal nor a **binding path**. An effect call
    /// is not an expression language: anything wanting a computation is a
    /// Module, referenced opaquely (Rule 46a).
    ///
    /// **A binding path is not an expression, and is admitted** - `{id}`,
    /// `{props.user.name}` lower to [`Expr::Get`], the same distinct
    /// first-class form [`AttrValue::Binding`] already is for an ordinary
    /// attribute. What stays refused is everything that computes: a call, an
    /// arithmetic expression, a template literal, an arrow function, an object
    /// or array literal. That line is the whole of Rule 46a and it has not
    /// moved; what moved is that a *path* was never on the computing side of
    /// it, and treating it as one meant a row's own key could not be handed to
    /// an effect at all.
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
    /// `fx.frobnicate(..)` is a member expression and [`Expr::Call`]'s callee
    /// is a flat `String`; encoding `"fx.frobnicate"` into it would be
    /// structure smuggled into a name. Named imports only, until a callee
    /// carries a path properly.
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
    /// offer the effect surface at all (Rule 49's `enable_host_imports`).
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
    /// `handlers = { onGrommet: frobnicate("x") }` would reach an element as *no
    /// attribute at all*. The attribute loop never sees an `on..` name, so
    /// every refusal above is blind to it, and the effect is erased by exactly
    /// the route the [`AttrValue::ImportedCall`] producer exists to close.
    ///
    /// It could not be honoured even if it were resolvable: an attribute set
    /// spread from a value cannot be checked against declared props, so
    /// accepting one would be a second, unchecked way to give an element
    /// attributes.
    SpreadAttribute {
        /// The tag it was written on.
        tag: String,
    },
    /// An ordinary JSX object binding cannot be represented by the owned
    /// expression vocabulary. Unlike an event refusal, this names the
    /// attribute syntax itself and never changes `ImportedCall` semantics.
    BindingSyntax {
        /// The ordinary attribute carrying the rejected expression.
        attr: String,
        /// Why the expression has no owned representation.
        message: String,
    },
    /// A `{...}` **child** the element tree has no node for: a call, a
    /// conditional, an object literal, a spread.
    ///
    /// # It was a silent drop, and that is the defect this closes
    ///
    /// The child positions this tree CAN hold are few and named:
    /// [`Node::Text`] (a string or a plain template literal),
    /// [`Node::Expr`] (a binding path), [`Node::Element`] (an element, or the
    /// element a list render arrow returns) and [`Node::Comment`]. Everything
    /// else used to be read, found unrepresentable, and dropped - the parse
    /// succeeded, the child was gone, and nothing anywhere said so. A
    /// `<Content>` whose only child was `{{greeting}}` (a JS object literal,
    /// not the `{{ }}` placeholder its author meant) came out EMPTY and looked
    /// exactly like a `<Content>` written empty on purpose.
    ///
    /// That is the shape Rule 10 exists to forbid, and it is the same rule the
    /// attribute side has honoured all along: an `on..` value that is not a
    /// call is [`EffectError::NotACall`], and a spread is
    /// [`EffectError::SpreadAttribute`], never a quietly missing attribute.
    ///
    /// # Refusing is not the same answer as reading
    ///
    /// A refusal says the tree has no node for this, which for `{f(x)}` or
    /// `{a && b}` is the whole truth - the graph is one-way data flow, not an
    /// expression language, and a child that computes belongs in a Module. For
    /// some forms it is only the truth FOR NOW: `{(props.a)}` is a path with
    /// parentheses around it and `{<Row/>}` is an element. Those are candidates
    /// for a reader, not permanent refusals, and until one exists this variant
    /// is what names them (see the parser's `child_form`).
    UnreadableChild {
        /// The tag it was written under, or `None` for a child of a top-level
        /// fragment (which has no tag to name).
        ///
        /// A screen is a hundred lines of nested elements and the form alone
        /// does not say WHERE, so the refusal carries the one piece of context
        /// the conversion already holds - the same choice
        /// [`EffectError::SpreadAttribute`] made.
        tag: Option<String>,
        /// What was written, named by FORM - "a call", "an object literal", "a
        /// conditional (`?:`)" - not the source text, which the parse does not
        /// carry this far.
        form: String,
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
            Self::NotAHostImport {
                attr,
                callee,
                source,
            } => write!(
                f,
                "`{attr}` calls `{callee}`, imported from `{source}`, which is compiled rather than granted"
            ),
            Self::GrantedWithoutScheme { source } => write!(
                f,
                "`{source}` is granted as a host namespace and is not spelled as one (`{HOST_PREFIX}...`)"
            ),
            Self::ArgCount {
                attr,
                effect,
                declared,
                required,
                given,
            } => write!(
                f,
                "`{attr}` calls `{effect}` with {given} arguments and it declares {declared}, of which {required} are required"
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
            Self::BindingSyntax { attr, message } => {
                write!(f, "`{attr}` has an unsupported binding expression: {message}")
            }
            Self::UnreadableChild { tag, form } => {
                match tag {
                    Some(tag) => write!(f, "<{tag}> has a child that is {form}")?,
                    None => write!(f, "a fragment has a child that is {form}")?,
                }
                write!(
                    f,
                    ", and the element tree has a node only for text, a binding path, an element and a comment"
                )
            }
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
    /// verbatim text, including any `{{ }}` Markdown-templating placeholders.
    /// Reading those is [`crate::template::substitute`], which lives beside
    /// this node for the reason its module doc gives: the placeholders are
    /// part of this variant's content, so the scan is not any one renderer's.
    Text(String),
    /// A `{binding}` expression child — a data-binding path.
    Expr(String),
    /// An authored **comment**, verbatim, delimiters included: `// like this`
    /// or `/* like this */`.
    ///
    /// # It contributes no ink and no box
    ///
    /// A comment is the one node kind that must reach the graph and *never*
    /// reach a frame. Every walker that lays out, draws, hit-tests, counts rows
    /// or projects to the ECS skips it, and that is asserted mechanically
    /// rather than by inspection: `libhbui/tests/comments_are_invisible.rs`
    /// parses one source twice - once retaining comments and once not - and
    /// requires the two `DrawList`s to be byte-identical. A walker that turned
    /// a comment into a layout node, or worse rendered it as text, diverges the
    /// two lists.
    ///
    /// # Retained only when the parse was asked to
    ///
    /// [`crate::ParseCtxBuilder::retain_comments`] is off by default, so the
    /// PUBLISH path (which never asks) produces documents that cannot contain
    /// this variant at all. The editor path asks, because the source it shows
    /// is emitted from the graph ([`crate::emit_tsx_document`]) and a
    /// comment-free graph would emit a gutted file: `data/projects/default/
    /// screens/home.tsx` is 23 of 52 lines comment.
    ///
    /// # Why verbatim, delimiters and all
    ///
    /// So that emit is a copy and the round trip is exact. The alternative -
    /// storing the content and re-deriving a delimiter - has to decide whether
    /// a block comment becomes one line comment or several, whether adjacent
    /// line comments were one comment or two, and where a blank line went; each
    /// of those decisions is a way for `parse -> emit -> parse` to stop being
    /// the identity. One authored comment is one `Comment`, spelled the way it
    /// was written.
    ///
    /// # Position in the enum is load-bearing
    ///
    /// LAST, and it must stay last. `libhbui::codec` serializes this enum with
    /// postcard, which writes a variant's INDEX; appending is the only change
    /// that leaves the existing three indices where previously-encoded data
    /// expects them.
    Comment(String),
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
    ///
    /// **A [`Node::Comment`] root is skipped, not counted.** A source whose
    /// header comment was retained is still one definition - it exposes one
    /// element - so counting the comments as extra roots would refuse a source
    /// that is exactly what this constructor is for. What that costs is
    /// stated rather than hidden: a `Definition` has no slot for a comment and
    /// this drops them. It is the honest place for that loss, because a
    /// `Definition` is the PUBLISHED unit and the publish path parses with
    /// [`crate::ParseCtxBuilder::retain_comments`] off - so on that path there
    /// is nothing here to drop. The path that keeps comments keeps them on the
    /// [`TsxDocument`], which is what [`crate::emit_tsx_document`] reads.
    pub fn from_document(
        symbol: impl Into<String>,
        doc: TsxDocument,
        interfaces: Vec<InterfaceDecl>,
    ) -> Result<Self, DefError> {
        let TsxDocument { root_nodes, imports } = doc;
        let mut roots = root_nodes
            .into_iter()
            .filter(|n| !matches!(n, Node::Comment(_)));
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
    /// A generic type application, preserving its constructor and every
    /// argument in source order (`Result<T, E>`, `PartialData<T, K>`, ...).
    ///
    /// **Appended last on purpose.** TypeShape is persisted positionally.
    Apply {
        constructor: String,
        args: Vec<TypeShape>,
    },
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
            // A DELIBERATELY MADE-UP HOST IMPORT (Rule 52). This sample used to
            // declare `navigate`, which is libhbui's vocabulary, in libtsx's own
            // documentation - and a sample is what the next reader copies. The
            // name below is obviously a placeholder precisely so nothing here
            // reads as a statement about what a real host grants.
            //
            // WHAT IS CHECKED, AND WHAT IS NOT. The signature check lives in
            // `parse::imported_call_attr` and covers an effect **attribute**:
            // an `on..` attribute's value is resolved through its `ImportDecl`
            // to a namespace and an exported name, and its arguments checked
            // against the `FuncSig` the provider grants. Nothing checks a
            // `HandlerDecl` body - no pass walks `Stmt`/`Expr::Call` and looks
            // the callee up in a signature list - so the call below agrees with
            // this signature only because the test named at the end of this
            // comment asserts it. Every libtsx and libhbui test stayed green
            // with the two disagreeing.
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
                name: "exampleHostCall".into(),
                params: vec![FieldDecl {
                    name: "subject".into(),
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
                        callee: "exampleHostCall".into(),
                        args: vec![Expr::LitStr("an example subject".into())],
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
    /// disagree with itself. `sample_module` carried a signature declaring an
    /// `S32` parameter beside a call passing a `LitStr` with every test in the
    /// workspace green.
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
                    // A placeholder vocabulary, deliberately (Rule 52): a
                    // fixture in libtsx that spelled an embedding's real effect
                    // would be the leak this crate's tests exist to catch.
                    "onGrommet".into(),
                    AttrValue::ImportedCall(ImportedCall {
                        namespace: "host:zork".into(),
                        name: "frobnicate".into(),
                        args: vec![Expr::LitStr("sprocket".into())],
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
    fn a_retained_comment_is_not_a_second_root() {
        // A source whose header comment was kept is still ONE definition: it
        // exposes one element. Counting the comments as roots would refuse
        // exactly the sources this constructor exists for - every authored
        // Highbay screen has a header block.
        let mut doc = a_document();
        doc.root_nodes.insert(0, Node::Comment("// the header".into()));
        doc.root_nodes.push(Node::Comment("// a trailing note".into()));

        let def = Definition::from_document("UserCard", doc, vec![]).expect("still one definition");
        assert_eq!(def.ui.tag, "Widget", "the element is the one that was found");

        // Two elements is still two, comments or no comments.
        let mut two = a_document();
        two.root_nodes.insert(0, Node::Comment("// the header".into()));
        two.root_nodes.push(Node::Element(Element {
            tag: "Stowaway".into(),
            type_args: vec![],
            attrs: vec![],
            children: vec![],
        }));
        assert_eq!(
            Definition::from_document("UserCard", two, vec![]),
            Err(DefError::SeveralRoots(2)),
            "the count is of ELEMENTS, so the comment neither adds to it nor hides a second root"
        );

        // And a document that is only comments has no UI at all.
        let only = TsxDocument {
            root_nodes: vec![Node::Comment("// nothing but a note".into())],
            imports: vec![],
        };
        assert_eq!(
            Definition::from_document("UserCard", only, vec![]),
            Err(DefError::NoUi)
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

    /// **The scheme is shape, not a name** (Rule 52). It is what lets the parse
    /// tell Rule 48's three answers apart *before* it consults a provider, and
    /// it is the only thing about a host namespace this crate knows: what lives
    /// under the scheme is the embedding's model.
    ///
    /// The near misses are the point - a display name, a relative path and a
    /// scoped package all name something with a source behind it.
    #[test]
    fn a_host_namespace_is_told_apart_by_its_scheme() {
        assert!(is_host_namespace("host:zork"));
        assert!(is_host_namespace("host:anything-at-all"));
        assert!(!is_host_namespace("Library Feed"));
        assert!(!is_host_namespace("./widgets/UserCard"));
        assert!(!is_host_namespace("@highbay/effects"));
        assert!(!is_host_namespace("hosted:effects"));
        assert!(!is_host_namespace(""));
    }

    #[test]
    fn owned_binding_ir_round_trips_through_serde() {
        let program = BlockArrow {
            params: vec![BindingParam {
                name: "input".into(),
                ty: TypeShape::Apply {
                    constructor: "Result".into(),
                    args: vec![TypeShape::Named("User".into()), TypeShape::Named("Error".into())],
                },
            }],
            body: vec![
                BlockStmt::Let {
                    slot: "rows".into(),
                    value: BindingExpr::Array(vec![
                        BindingExpr::Literal(BindingLiteral::Bool(true)),
                        BindingExpr::Path(vec!["input".into(), "rows".into()]),
                    ]),
                },
                BlockStmt::Await {
                    slot: Some("saved".into()),
                    awaitable: BindingExpr::Call {
                        namespace: "storage".into(),
                        name: "save".into(),
                        type_args: vec![TypeShape::Named("User".into())],
                        args: vec![BindingExpr::Path(vec!["input".into()])],
                    },
                },
                BlockStmt::If {
                    condition: BindingExpr::Path(vec!["saved".into(), "ok".into()]),
                    then_branch: vec![BlockStmt::Return(BindingExpr::Literal(
                        BindingLiteral::Null,
                    ))],
                    else_branch: vec![BlockStmt::Try {
                        body: vec![BlockStmt::Return(BindingExpr::Path(vec![
                            "saved".into(),
                            "error".into(),
                        ]))],
                        error_slot: "error".into(),
                        catch: vec![BlockStmt::Return(BindingExpr::Literal(
                            BindingLiteral::String("failed".into()),
                        ))],
                    }],
                },
            ],
        };
        let value = AttrValue::BindingExpr(BindingExpr::Async(program));
        let json = serde_json::to_string(&value).expect("serialize owned binding");
        let back: AttrValue = serde_json::from_str(&json).expect("deserialize owned binding");
        assert_eq!(value, back);
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

    /// **Every registered spelling survives the round trip**, in both
    /// directions and for both enums.
    ///
    /// The half worth pinning is `intern(as_str(x)) == x`: an interned variant
    /// whose constant disagreed with its `as_str` arm would encode as one
    /// identifier and decode as a different variant, which is the silent
    /// corruption a positional encoding was rejected to avoid.
    #[test]
    fn an_interned_spelling_round_trips_through_its_identifier() {
        for symbol in [
            ObjectSymbol::It,
            ObjectSymbol::ListItem,
            ObjectSymbol::Design,
            ObjectSymbol::UiSite,
            ObjectSymbol::UiComponent,
            ObjectSymbol::Named("whateverElse".into()),
        ] {
            assert_eq!(ObjectSymbol::intern(symbol.as_str()), symbol);
        }
        for accessor in [
            PropertyAccessor::Value,
            PropertyAccessor::Id,
            PropertyAccessor::Index,
            PropertyAccessor::Props,
            PropertyAccessor::IsAuthoring,
            PropertyAccessor::Named("customerRef".into()),
        ] {
            assert_eq!(PropertyAccessor::intern(accessor.as_str()), accessor);
        }
    }

    /// **The encoding is the identifier, and a PROMOTION changes no byte.**
    ///
    /// The whole point of the hand-written impls (see [`ObjectSymbol`]'s doc):
    /// `Named("it")` and `It` encode identically, so moving an identifier into
    /// the interned set leaves every committed graph readable and equal. Under
    /// a derived `Serialize` these two would encode as different variant
    /// indices, and the promotion would silently make stored bytes and a fresh
    /// parse two different values for one authored expression.
    #[test]
    fn a_symbol_encodes_as_its_identifier_whether_or_not_it_is_interned() {
        let interned = serde_json::to_string(&ObjectSymbol::It).expect("serialize");
        let named =
            serde_json::to_string(&ObjectSymbol::Named("it".into())).expect("serialize");
        assert_eq!(interned, "\"it\"");
        assert_eq!(interned, named, "a promotion would have changed the bytes");
        assert_eq!(
            serde_json::from_str::<ObjectSymbol>(&named).expect("deserialize"),
            ObjectSymbol::It,
            "a registered identifier decodes onto its interned variant",
        );
        assert_eq!(
            serde_json::from_str::<ObjectSymbol>("\"somethingElse\"").expect("deserialize"),
            ObjectSymbol::Named("somethingElse".into()),
            "an unregistered identifier decodes without loss",
        );

        assert_eq!(
            serde_json::to_string(&PropertyAccessor::IsAuthoring).expect("serialize"),
            "\"isAuthoring\"",
        );
        assert_eq!(
            serde_json::from_str::<PropertyAccessor>("\"props\"").expect("deserialize"),
            PropertyAccessor::Props,
        );
    }
}
