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
//! * **Event bindings** ([`AttrValue::BindingExpr`] carrying a
//!   [`BindingExpr::Call`]) - `onGrommet={frobnicate("sprocket")}`: an
//!   [`is_event_binding`] attribute whose value is ONE call resolving to a
//!   granted host import. Deliberately *not* a handler and not a body - see
//!   [`BindingExpr::Call`] and [`HOST_PREFIX`] (LIBHBUI_PLAN Rules 46, 46a,
//!   48). There is no variant of its own: an imported call in expression
//!   position is an ordinary call, and whether the thing on the other end is
//!   an EFFECT is the embedding's reading (Rule 52).
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

/// A TS `interface` declaration: `interface Name extends … { fields… }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    /// The `extends` clause, one [`TypeShape::Extends`] per heritage entry, in
    /// source order.
    ///
    /// **A TYPE EXPRESSION, not a name list** (Tim, 2026-08-27: *"encode it as
    /// a type expression node Extends{base}"*). `extends Omit<ContainerProps,
    /// "direction">` is the whole point of the clause for this project, and a
    /// `Vec<String>` could not hold it - the base is an expression that has to
    /// be evaluated like any other, by the one evaluator.
    ///
    /// `#[serde(default)]` so every already-committed definition, which has no
    /// such clause, still decodes.
    #[serde(default)]
    pub extends: Vec<TypeShape>,
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
    /// An owned object/data binding expression - and, since the event lowering
    /// re-pointed here, **the shape an `on..` attribute takes too**.
    ///
    /// This is deliberately separate from [`Binding`](AttrValue::Binding),
    /// which is the legacy path spelling.
    ///
    /// # `ImportedCall` was here, and it is subsumed
    ///
    /// An `ImportedCall { namespace, name, args }` variant sat between
    /// [`Opaque`](AttrValue::Opaque) and this one and carried the narrow event
    /// grammar. It is gone, subsumed the way `Member` was subsumed by
    /// [`MemberOf`](BindingExpr::MemberOf): an imported call in expression
    /// position is an ordinary [`BindingExpr::Call`], which already carries a
    /// `namespace`, a `name` and `args` - **and a `type_args` the removed
    /// variant had nowhere to put**. `frobnicate<Sprocket>("x")` and
    /// `frobnicate("x")` captured as the same value under the old shape, which
    /// is the identical erasure [`Element::type_args`] exists to prevent for
    /// `<List<Message>>`.
    ///
    /// What does NOT move down here is the CHECKING: an `on..` attribute's
    /// value is still resolved through the module's import chain against the
    /// signature the embedding declares ([`ParserHost`]), and still refused
    /// with an [`EffectError`] when it is not one call to a granted host
    /// import. Only the carrier changed. Whether the thing on the other end is
    /// an EFFECT stays the embedding's reading (Rule 52) - the parser reads a
    /// call.
    ///
    /// **A removal costs a version bump**, because postcard encodes enum
    /// variants by index with no names in the bytes: this variant moved from
    /// index 6 to index 5, so `libhbui`'s `HBDEF_VERSION` is 4 for it.
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

/// **A literal accepted by an owned object/data binding expression, at the
/// width it was captured or declared at.**
///
/// # An integer is never silently a float
///
/// This replaced a `BindingLiteral` whose numeric arm was a single
/// `Number(f64)`, and the whole of the change is that one arm becoming four.
/// Tim, 2026-08-23: *"we should never silently encode integer as floats."* An
/// f64 carries every i32 exactly and stops carrying i64 at 2^53, so a lone
/// `Number` made "how wide was this, and was it whole?" unanswerable AFTER the
/// fact - a row id past 2^53 arrives rounded, and nothing downstream can tell a
/// rounded id from an id.
///
/// Exactness is not new here either: the sibling event vocabulary [`Expr`] has
/// carried `LitS32`/`LitS64`/`LitF32`/`LitF64` since it existed, and `lower_arg`
/// in `parse.rs` refuses a literal its declared type cannot hold rather than
/// rounding it. `Number(f64)` was the odd one out, not the precedent.
///
/// # The names, and the ONE place they meet [`TypeShape`]'s
///
/// [`TypeShape`] spells its widths WIT's way - `S32`/`S64`/`F32`/`F64`, where
/// `S` is SIGNED (WIT writes `s32`/`u32` where Rust writes `i32`/`u32`) - at
/// well over a hundred sites. These are spelled Rust's way, which is what an
/// author of a literal reads. Two vocabularies for one ladder is a translation
/// waiting to be written twice, so it is written ONCE:
/// [`type_shape`](LiteralValue::type_shape) going up and
/// [`narrow`](LiteralValue::narrow) coming down. No consumer maps a width by
/// hand.
///
/// # There is no `Null`, and that is a distinction rather than a removal
///
/// A written `null` is a SOURCE FORM - an author typed the token, and libtsx
/// captures what an author wrote - so it stays recordable, as
/// [`BindingExpr::Null`]. What it is NOT is a value with a type, which is what
/// every variant here is; absence as MODELLING is the type vocabulary's job and
/// [`TypeShape::Option`] already does it. Tim, 2026-08-23: *"it would be better
/// to use BindingExpr::Optional<T> instead of null, but null will still need a
/// way to be recorded."* Keeping `Null` here would have made every consumer
/// asking "what width is this literal?" answer "none, sometimes", which is the
/// two-questions-one-type shape this split exists to end.
///
/// # APPEND-LAST from here
///
/// This enum reaches disk inside [`BindingExpr::Literal`] and is persisted
/// POSITIONALLY by postcard, so a new variant goes at the END - see
/// [`BindingExpr`]'s own note for what a removal costs instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LiteralValue {
    Bool(bool),
    /// A whole number that FITS in 32 bits - what a field declaring
    /// [`TypeShape::S32`] produces, never a default anything narrows into.
    Int32(i32),
    /// A whole number. **This is what the parser captures an integral literal
    /// as**, whatever it is later declared to be. Tim, 2026-08-23: *"it's safe
    /// to have integers as Int64, then map to floats only if the props require
    /// a float. 64 bit ints/floats are cheap for us and provide the most
    /// compatibility."*
    Int64(i64),
    /// A 32-bit float - again, what a field DECLARING it produces.
    Float32(f32),
    /// A fractional number, and what the parser captures a decimal literal as.
    Float64(f64),
    String(String),
}

impl LiteralValue {
    /// **The [`TypeShape`] this literal already IS** - the up-hill half of the
    /// one mapping between the two width vocabularies (see the type doc).
    pub fn type_shape(&self) -> TypeShape {
        match self {
            Self::Bool(_) => TypeShape::Bool,
            Self::Int32(_) => TypeShape::S32,
            Self::Int64(_) => TypeShape::S64,
            Self::Float32(_) => TypeShape::F32,
            Self::Float64(_) => TypeShape::F64,
            Self::String(_) => TypeShape::String,
        }
    }

    /// **This literal as an `f64`, or `None` when it is not a number.**
    ///
    /// The READ side, for a consumer whose own value type is `f64` - a layout
    /// metric, a scalar attribute, a runtime number. It is deliberately the
    /// ONLY such conversion: a consumer that wrote `as f64` at its own match
    /// site would be re-deciding, per site, a question this type answers once.
    ///
    /// **Lossy past 2^53, and that loss belongs to the CONSUMER's type, not to
    /// the capture.** The literal still holds the exact integer; what is
    /// narrowing here is the `f64` the caller asked for. That is the whole
    /// difference from the vocabulary this replaced, where the capture itself
    /// was an `f64` and the exact value was gone before any consumer saw it.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int32(value) => Some(f64::from(*value)),
            Self::Int64(value) => Some(*value as f64),
            Self::Float32(value) => Some(f64::from(*value)),
            Self::Float64(value) => Some(*value),
            Self::Bool(_) | Self::String(_) => None,
        }
    }

    /// **This literal AT the declared width, or `None` when it does not fit
    /// exactly** - the down-hill half, and the only place a capture width
    /// becomes a declared one.
    ///
    /// The refusal is the point and it is [`lower_arg`](crate::parse)'s
    /// contract restated for the binding vocabulary: a literal the declared
    /// type cannot hold is refused, never rounded, truncated or wrapped, so a
    /// silently shortened id fails at the source text that caused it rather
    /// than somewhere with no view of it. 2^53 is where an f64 stops being the
    /// integer it was written as, which is why [`TypeShape::S64`] is bounded
    /// there and not at [`i64::MAX`].
    ///
    /// **`F32` is a CAST and not a refusal**, deliberately: that is what the
    /// event grammar has always done for a declared `F32`, and tightening it
    /// here would change which sources parse - a decision with its own evidence
    /// to gather, not a side effect of moving the widths into one place.
    ///
    /// `U32`/`U64` have no authored spelling (see [`TypeShape`]), so no
    /// declaration can name one and there is nothing for an arm here to serve.
    pub fn narrow(&self, declared: &TypeShape) -> Option<Self> {
        /// 2^53 - past it an f64 literal is no longer the integer it was
        /// written as.
        const EXACT: i64 = 9_007_199_254_740_992;
        match (self, declared) {
            (Self::Bool(_), TypeShape::Bool) => Some(self.clone()),
            (Self::String(_), TypeShape::String) => Some(self.clone()),
            (Self::Int32(v), _) => Self::Int64(i64::from(*v)).narrow(declared),
            (Self::Int64(v), TypeShape::S32) => i32::try_from(*v).ok().map(Self::Int32),
            (Self::Int64(v), TypeShape::S64) => {
                (-EXACT..=EXACT).contains(v).then_some(Self::Int64(*v))
            }
            (Self::Int64(v), TypeShape::F32) => Some(Self::Float32(*v as f32)),
            (Self::Int64(v), TypeShape::F64) => Some(Self::Float64(*v as f64)),
            (Self::Float32(v), _) => Self::Float64(f64::from(*v)).narrow(declared),
            (Self::Float64(v), TypeShape::F32) => Some(Self::Float32(*v as f32)),
            (Self::Float64(v), TypeShape::F64) => Some(Self::Float64(*v)),
            // A fractional value is not a whole one, and rounding it here is
            // exactly what this function refuses to do.
            (Self::Float64(v), TypeShape::S32) => (v.fract() == 0.0
                && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(v))
            .then(|| Self::Int32(*v as i32)),
            (Self::Float64(v), TypeShape::S64) => (v.fract() == 0.0
                && (-(EXACT as f64)..=(EXACT as f64)).contains(v))
            .then(|| Self::Int64(*v as i64)),
            _ => None,
        }
    }
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
/// removal is why `libhbui`'s `HBDEF_VERSION` was 2. A future removal is another
/// such event and costs another bump; there is no cheaper way to take one.
///
/// **And there has now been a second**: `Member { base, path }` left in favour
/// of [`MemberOf`](BindingExpr::MemberOf), which subsumes it (Tim, 2026-08-23:
/// *"Member and MemberOf shouldn't both exist"*), in the SAME change that
/// appended [`SymbolValue`](BindingExpr::SymbolValue), [`MemberOf`] and
/// [`Null`](BindingExpr::Null) and reshaped [`LiteralValue`]. One change, one
/// bump: landing the addition and the removal separately would have cost two
/// bumps and left a window in which both member forms existed, which is the
/// duplication the removal was for. `HBDEF_VERSION` is 3 for it.
///
/// [`MemberOf`]: BindingExpr::MemberOf
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BindingExpr {
    Literal(LiteralValue),
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
    /// `a === b` and `a == b` - an equality test.
    ///
    /// **Appended after [`Cond`](BindingExpr::Cond) on purpose.**
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
    /// `!=`/`!==` are NOT this variant, and they are no longer refused: they
    /// are [`Ne`](BindingExpr::Ne), appended for the purpose. The refusal that
    /// stood here read *"a negation is a second operator, nothing has asked
    /// for it, and inferring it from an equality would be inventing syntax the
    /// author did not write"* - and every clause of it still holds. The second
    /// operator is a second VARIANT rather than a rewrite of this one, and
    /// nothing is inferred in either direction: `a != b` is `Ne`, `!(a == b)`
    /// is [`Not`](BindingExpr::Not) wrapping this, and neither is turned into
    /// the other. Only "nothing has asked for it" stopped being true (Tim,
    /// 2026-09-03: *"BindingExpr has to grow to support what we need,
    /// specifically for negation"*).
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
    /// **A top-level identifier CALLED as a value** - `it()`, `uiSite()`,
    /// `design()`.
    ///
    /// **Appended after [`Arrow`](BindingExpr::Arrow), in the same change that
    /// removed `Member`** - see the enum's note on what a removal costs, and
    /// `SCOPE_TREES.md` step 3 on why the two had to travel together.
    ///
    /// The form is a call to a bare identifier: no namespace, no type argument,
    /// no argument. Anything else is a [`Call`](BindingExpr::Call) and stays
    /// one - `it` alone is a NAME and lowers as the [`Path`](BindingExpr::Path)
    /// it is, `propsOf<T>()` carries a type argument, `ns.design()` is somebody
    /// else's `design`. That strictness is what keeps ONE source spelling to
    /// ONE shape: a lenient capture would give `it(x)` the bare reading and
    /// silently drop the argument the author wrote.
    ///
    /// **This says nothing about what the identifier MEANS.** Which scope
    /// answers `it()`, whether `design()` varies by surface, whether an
    /// identifier is known at all - every one of those is the consumer's
    /// question, asked with a scope in hand (`libhbui::attr`). See
    /// [`ObjectSymbol`] for why an identifier registry answers to this
    /// vocabulary's rule rather than breaching it.
    SymbolValue(ObjectSymbol),
    /// **ONE member hop off an expression** - the `isAuthoring` of
    /// `design().isAuthoring`.
    ///
    /// **Appended after [`SymbolValue`](BindingExpr::SymbolValue), and it is
    /// what `Member { base, path }` became.**
    ///
    /// # It NESTS; it does not carry a path
    ///
    /// `uiComponent().props.x` is
    /// `MemberOf(MemberOf(SymbolValue(UiComponent), Props), Named("x"))` - the
    /// one-hop form enclosing itself. Tim, 2026-08-23: *"a nested member
    /// accessor is the enclosed form recursively."* The `Vec<String>` the old
    /// variant carried made a chain a LIST hanging off one base, which is a
    /// second shape for something the enum can already express by recursion,
    /// and it forced the accessor to be a bare string where a hop off a hop
    /// wants to be an expression like any other. Resolution is then a fold -
    /// resolve the innermost expression, then walk outward one
    /// [`PropertyAccessor`] at a time - and one hop is the overwhelmingly
    /// common case in the corpus anyway.
    ///
    /// # Why `Member` could not stay beside it
    ///
    /// Tim, 2026-08-23: *"Member and MemberOf shouldn't both exist."* `Member`
    /// was added for the base that a [`Path`](BindingExpr::Path) cannot root -
    /// `design().isAuthoring`, whose base is a CALL - which is this variant's
    /// entire job. Two forms for member access is one authored spelling with
    /// two representations, and every consumer then owes both an arm.
    ///
    /// The base stays an expression rather than narrowing to a symbol: that is
    /// what makes the recursion work, and it is also what keeps a member on a
    /// [`Call`](BindingExpr::Call) result representable. A chain rooted at an
    /// IDENTIFIER is still a `Path` and must stay one - `props.value` has one
    /// shape, decided in `parse.rs`.
    ///
    /// Computed access (`a[b]`) is not this variant and stays refused.
    MemberOf(Box<BindingExpr>, PropertyAccessor),
    /// **The written token `null`.**
    ///
    /// **Appended after [`MemberOf`](BindingExpr::MemberOf).** It arrived here
    /// from `BindingLiteral::Null` when that enum became [`LiteralValue`], and
    /// the move is the distinction: a literal VALUE has a width and a type,
    /// and `null` has neither. What it has is a source form, and capturing
    /// source forms is this vocabulary's whole remit.
    ///
    /// **Absence as MODELLING is not this.** Tim, 2026-08-23: *"it would be
    /// better to use BindingExpr::Optional<T> instead of null, but null will
    /// still need a way to be recorded."* [`TypeShape::Option`] is where "this
    /// may be missing" is declared; this variant only records that an author
    /// typed the four characters. What a consumer MAKES of them - absence, a
    /// refusal, a runtime null - is the consumer's, and the consumers in this
    /// workspace already disagree, which is the evidence that it was never one
    /// meaning.
    Null,
    /// **The prefix `!`** - the authored negation, `!x`.
    ///
    /// **Appended after [`Null`](BindingExpr::Null), with
    /// [`Ne`](BindingExpr::Ne) beside it.** A pure append, so `HBDEF_VERSION`
    /// does not move: *"a variant APPENDED to an enum is the one shape that
    /// does not force a bump ... existing indices are untouched"*
    /// (`libhbui::codec`). The cost arrives the day an author WRITES one, per
    /// this enum's own rule above.
    ///
    /// Boxed because the operand is an ordinary expression, including another
    /// `Not`. `!!x` is `Not(Not(x))`, which is what the author wrote; nothing
    /// here collapses a double negation, because "these two cancel" is a
    /// MEANING and meanings belong to the lowering.
    ///
    /// **Only the LOGICAL not.** `-x`, `+x`, `~x`, `typeof x`, `void x` and
    /// `delete x` share oxc's unary node and stay refused by name: each is a
    /// different operator answering a different question, and none has been
    /// asked for. TypeScript's POSTFIX `!` (the non-null assertion) is a
    /// different node entirely and is refused where it is written.
    Not(Box<BindingExpr>),
    /// `a !== b` and `a != b` - an inequality test, mirroring
    /// [`Eq`](BindingExpr::Eq) field for field.
    ///
    /// **Appended after [`Not`](BindingExpr::Not) on purpose.**
    ///
    /// # Why this is a variant and not `Not(Eq { .. })`
    ///
    /// Because a serialization of an [`Element`] is *"the element as authored,
    /// which is what makes graph -> TSX a real direction rather than an
    /// aspiration"* - and `a != b` and `!(a == b)` are two things an author can
    /// write. Lowering the first into the second makes them ONE shape, and the
    /// emitter then has to guess which was written. That is the same loss
    /// `Eq`'s `strict` field exists to refuse, one operator along: capture the
    /// form, leave the meaning to the lowering.
    ///
    /// The meaning is expected to be one formula either way - a consumer
    /// evaluates `Ne` as a negated `Eq` - and that is a judgment for the
    /// consumer to make with a scope in hand, not a normalisation for this
    /// layer to bake into the capture. Nothing is inferred in EITHER
    /// direction: a `Not` wrapping an `Eq` stays that, and is not rewritten
    /// into this.
    ///
    /// `strict` is `true` for `!==` and `false` for `!=`, recorded for the
    /// reason `Eq` records it: the two operators differ exactly where coercion
    /// would happen, and folding them together answers that question here
    /// instead of leaving it to the consumer.
    Ne {
        left: Box<BindingExpr>,
        right: Box<BindingExpr>,
        /// `true` for `!==`, `false` for `!=`.
        strict: bool,
    },
}

impl BindingExpr {
    /// **A dotted NAME as the [`Path`](BindingExpr::Path) it is** -
    /// `"props.items"` is `Path(["props", "items"])`.
    ///
    /// The split and its inverse [`path_spelling`](BindingExpr::path_spelling)
    /// are written here, once. A `Path` is a name in TWO forms - segments in
    /// the vocabulary, a dotted string wherever a name is spelled - and a
    /// consumer splitting or joining at its own call site is a second formula
    /// for the same correspondence, with nothing to keep the two in step when
    /// one of them learns about, say, a segment containing a dot.
    pub fn path(spelling: &str) -> BindingExpr {
        BindingExpr::Path(spelling.split('.').map(str::to_owned).collect())
    }

    /// **The dotted name a [`Path`](BindingExpr::Path) SPELLS**, or `None` for
    /// anything that is not one. The inverse of [`path`](BindingExpr::path).
    pub fn path_spelling(&self) -> Option<String> {
        match self {
            BindingExpr::Path(segments) => Some(segments.join(".")),
            _ => None,
        }
    }

    /// **A member chain on `base`, one nested hop per segment** -
    /// `member_chain(<f()>, ["x", "y"])` is
    /// `MemberOf(MemberOf(<f()>, x), y)`.
    ///
    /// The nesting is written HERE and nowhere else, with
    /// [`member_path`](BindingExpr::member_path) as its exact inverse: a
    /// consumer that spelled the fold itself would be a second formula for the
    /// shape, and the two would drift in the direction that matters - one of
    /// them putting the segments back in the wrong order.
    ///
    /// An empty `path` answers `base` unchanged, which is what makes a bare
    /// symbol and a member read one expression rather than two cases.
    pub fn member_chain<'a>(
        base: BindingExpr,
        path: impl IntoIterator<Item = &'a str>,
    ) -> BindingExpr {
        path.into_iter().fold(base, |base, segment| {
            BindingExpr::MemberOf(Box::new(base), PropertyAccessor::intern(segment))
        })
    }

    /// **This expression's member chain taken apart**: the innermost base, and
    /// every accessor written on it in SOURCE order.
    ///
    /// The inverse of [`member_chain`](BindingExpr::member_chain), and the one
    /// walk down the nesting. An expression that is not a member access answers
    /// `(self, [])`.
    pub fn member_path(&self) -> (&BindingExpr, Vec<&str>) {
        let mut path = Vec::new();
        let mut cursor = self;
        while let BindingExpr::MemberOf(base, member) = cursor {
            path.push(member.as_str());
            cursor = base;
        }
        path.reverse();
        (cursor, path)
    }
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
    /// The only answer that supplies anything an `on..` attribute's call can
    /// resolve against.
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
    /// the route the event lowering exists to close.
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
    /// attribute syntax itself and never changes the event grammar's
    /// semantics.
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

/// The shape of a type as it crosses the edge.
///
/// **NOT 1:1 with WIT, and that is settled rather than aspirational.** This doc
/// said *"Maps 1:1 onto WIT types"* until 2026-09-06. It was already false —
/// `Pick`, `Omit`, `Extends` and `Apply` are type-level OPERATORS with no WIT
/// counterpart, and `IndexedAccess` is a fifth — and it cannot be made true:
/// WIT has records, variants, enums, flags, lists, options, results, tuples and
/// resources, and NO lookup or mapped types at all. Tim, on being shown that:
/// *"that's a limitation of WIT we can't overcome, so the 1:1 goes, but we have
/// a deterministic one-way spelling."*
///
/// **So the relation is a DETERMINISTIC ONE-WAY REDUCTION, not a bijection.**
/// Every `TypeShape` reduces to exactly one WIT type, always the same one; no
/// WIT type reduces back. The operators exist to be EVALUATED
/// (`libhbdata::typeexpr`) before anything is emitted, so what reaches
/// `nocap-witgen` is already a WIT type. An operator arriving at the edge
/// unreduced is a defect, not a spelling to invent a WIT form for — inventing
/// one would put a name-encoded relationship across the ABI boundary, which is
/// the worst place for it.
///
/// **`S` is SIGNED - WIT's spelling, not Rust's.** WIT writes the pair as
/// `s32`/`u32` where Rust writes it `i32`/`u32`, so [`TypeShape::S32`] and Rust
/// `i32` are the same type, and [`TypeShape::U32`] is the other half of that
/// pair. Recorded here because the letter reads like an abbreviation for
/// "scalar" or "size" to anyone who has not met WIT, and the wrong reading
/// costs a sign.
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
    // THE UNSIGNED PAIR. Both **appended last**, for the reason `Apply` states:
    // TypeShape is persisted positionally, so a variant inserted next to `S32`
    // where it reads better would change what every already-committed byte
    // decodes to. Appending keeps old bytes readable, which is the whole of the
    // placement rule.
    //
    // WHY UNSIGNED EXISTS AT ALL. This is the umbrella type consumed by
    // `nocap-witgen`, and the WIT it generates is already unsigned:
    // `deps/libnocap/wit/state.wit` and `net.wit` use `u8` (`list<u8>` payloads
    // and bodies), `u16` (an HTTP status), and `u64` - the last carrying the
    // `{hi, lo}` pair a 128-bit nocap handle is split into, because "WIT has no
    // u128" (state.wit's own header). Without these variants the vocabulary
    // cannot describe the interface it emits, and every such value would have
    // to arrive as `S32`/`S64`. An unsigned value silently becoming signed is
    // the same class of defect as an integer silently becoming a float: the
    // declaration is quietly replaced by a different one, and the loss shows up
    // as a wrapped handle half or a negative length far from here.
    //
    // THE AUTHORING SPELLING IS DEFERRED, NOT OMITTED. `type_shape` in
    // `parse.rs` reaches this enum from TS source in exactly two places -
    // `TSBigIntKeyword => S64`, and plain `number` defaulting to `F64` - so
    // `S32`, `F32` and now `U32`/`U64` have NO authored spelling and are
    // reachable only when a TypeShape arrives from somewhere other than TS
    // text. TypeScript has neither unsigned types nor width annotations, so a
    // spelling needs a convention the language does not supply: a branded
    // alias, a declared alias the system recognises, or a decorator. The
    // capability is wanted; no host language hands us the spelling; the choice
    // of which is deliberately left open rather than guessed at here, because
    // an authoring convention invented to fill a blank is the one thing a
    // corpus of user source cannot later be talked out of.
    /// WIT `u32`. No authored spelling yet - see the note above.
    U32,
    /// WIT `u64`, the width the generated interface leans on hardest: a
    /// nocap handle crosses as two of these. No authored spelling yet - see
    /// the note above.
    U64,
    // THE KEY OPERATORS. Appended last, same rule as everything above.
    //
    // WHY THESE TWO AND NOT `Partial`. The line is not "which utility types
    // matter" - it is WHAT KIND OF ARGUMENT THE OPERATOR TAKES. `Apply` holds
    // `args: Vec<TypeShape>`, so it can carry any operator whose arguments are
    // TYPES: `Result<A, B>` is faithful there, and so is `Partial<X>`, which is
    // why `Partial` gets no variant. It is an application of Optional to each
    // of X's fields, and `Option` already spells that; the reduction belongs to
    // resolution, not to a node here.
    //
    // `Omit` and `Pick` take FIELD NAMES. `Apply` demands a type in that slot,
    // so the parser has to invent one, and it does: `type_shape` falls to its
    // `_ =>` arm and produces `Named("unknown")`. MEASURED, before this
    // existed:
    //
    //     Omit<ContainerProps, "direction">
    //       -> Apply { "Omit", [Named("ContainerProps"), Named("unknown")] }
    //
    // The name is not merely unresolved, it is GONE - two Omits over one base
    // that hide different fields decode to identical bytes. These variants are
    // not extra expressiveness; they are the repair of a node that cannot hold
    // a name where it requires a type.
    //
    // UNRESOLVED IS THE POINT. Both are the AUTHORED form and both survive into
    // `.hbtypes` unreduced, because reducing them needs the base's field list
    // and that may live in a layer this pack never loaded. A type expression
    // tree resolves the way any expression tree does (Tim, 2026-08-27), and a
    // key naming no field of the resolved base is rejected THERE - one place,
    // with the whole document in hand, rather than here with a fragment of it.
    /// `Omit<Base, "a" | "b">` - Base without the named fields.
    ///
    /// `base` is boxed rather than a bare name so the operators compose:
    /// `Omit<Omit<X, "a">, "b">` and `Pick<Omit<X, "a">, "b">` are both this
    /// tree, nested.
    ///
    /// `omitted` is `Vec<String>` and not `Vec<TypeShape>` on purpose. A field
    /// name is not a type, and a slot that can hold a type is a slot that can
    /// hold `Named("unknown")` - which is the exact defect this variant exists
    /// to fix. The narrower field makes that state unrepresentable.
    Omit {
        base: Box<TypeShape>,
        omitted: Vec<String>,
    },
    /// `Pick<Base, "a" | "b">` - Base with ONLY the named fields.
    ///
    /// The dual of [`TypeShape::Omit`]: same shape, opposite fold. See its
    /// documentation for why the key list is `Vec<String>`.
    Pick {
        base: Box<TypeShape>,
        picked: Vec<String>,
    },
    /// **One entry of an `extends` clause** - `interface P extends Base {}`.
    ///
    /// # It was DROPPED SILENTLY, and that was the blocker
    ///
    /// `interface HBoxProps extends Omit<ContainerProps, "direction"> {}` is
    /// the idiomatic TypeScript for exactly what a container's props are, and
    /// `convert_interface` had no arm for the clause: it produced an interface
    /// with NO FIELDS and no error. A container packed from one carried an
    /// empty shape, so `#[container]`'s fixed-attribute check would have
    /// compared against the empty set and passed anything - a check reporting
    /// success having checked nothing. That is why the check shipped absent.
    ///
    /// # Why a node and not a `Vec<String>` on the interface
    ///
    /// Because the useful cases are not names. `extends Omit<..>`,
    /// `extends Pick<..>` and `extends Partial<..>` all have to be admissible,
    /// and each is an expression that has to be evaluated the way every other
    /// expression is - by the one evaluator, with the same key checking and the
    /// same rejection of an unknown base. Storing a name would need a second,
    /// weaker resolution path beside it.
    ///
    /// **It evaluates to its base's fields**, which the interface's own fields
    /// are then merged onto. See `libhbdata::typeexpr`.
    Extends {
        base: Box<TypeShape>,
    },
    // THE INDEXED ACCESS. Appended last, same rule as everything above: this
    // enum is persisted positionally, so it goes at the END and not beside
    // `Omit` and `Pick` where it reads better. Placing it there would renumber
    // `Pick` and `Extends` and change what every already-committed byte decodes
    // to; appending keeps old bytes readable, which is the whole of the
    // placement rule.
    //
    // WHY ITS OWN VARIANT AND NOT `Apply`, which is `Omit`/`Pick`'s reason
    // exactly: `Apply` holds `args: Vec<TypeShape>` and DEMANDS A TYPE in every
    // slot. `Person["handle"]` takes a TYPE and a KEY LITERAL, so the type slot
    // would have to hold a name - and a slot that can hold a type is a slot
    // that can hold `Named("unknown")`.
    //
    // WHY NOT A `Named` HOLDING THE BRACKETS, which is what it WAS. MEASURED,
    // before this variant existed:
    //
    //     Person["handle"]  ->  Named("Person[\"handle\"]")
    //
    // and it did not stop there: `libhbdata::typeexpr::eval` carried that
    // string into the FINAL vocabulary as `ShapeType::Named("Person[\"handle\"]")`,
    // a name no declaration answers, where the field's own type belonged. The
    // relationship was TEXT INSIDE A NAME - the shape RULING 4 forbids ("the
    // convention is for the TS spelling, the nodegraph is explicit in the
    // relationship") - it survived the strict lowering, and no gate said a
    // word. Every author of an indexed access got that by default.
    //
    // WHAT IT IS FOR. A form is a PROJECTION over a raw type, and this is the
    // spelling that keeps the link to the raw type WITHOUT CHANGING THE FORM'S
    // SHAPE. `Pick<Person, "handle">` also names Person, but it evaluates to
    // `{ handle: string }` - a record - so a form spelled that way gains a
    // level of nesting per projected field. `Person["handle"]` evaluates to
    // `string`: the provenance lives in the spelling and the shape does not
    // move. That contrast is the whole reason this exists and is asserted as
    // one test in `crates/libhbdata/tests/indexed_access.rs`.
    /// `Base["key"]` - TypeScript's indexed access. **The field's own type,
    /// named through the type that declares it.**
    ///
    /// `base` is boxed so the operators compose in both directions -
    /// `Pick<Person["address"], "city">` is the spelling the corpus already
    /// writes, and `Person["address"]["city"]` is this node nested. A bare name
    /// here would refuse the second one.
    ///
    /// `key` is a `String` and not a `TypeShape` for [`TypeShape::Omit`]'s
    /// stated reason: a field name is not a type, and the narrower field makes
    /// `Named("unknown")` in that slot unrepresentable.
    ///
    /// **A key naming no field of the base is NOT rejected here.** Like
    /// `Omit`/`Pick`, this is the AUTHORED form and survives unreduced, because
    /// checking the key needs the base's field list and that may live in a
    /// layer this pack never loaded. It is rejected at evaluation, with the
    /// whole document in hand - `libhbdata::typeexpr`, one place.
    IndexedAccess {
        base: Box<TypeShape>,
        key: String,
    },
    // THE LITERAL TYPE AND THE UNION. Appended last, same rule as everything
    // above and for the same reason: this enum is persisted positionally, so a
    // variant that reads better beside `String` or beside `Option` still goes
    // at the END. `deps/libtsx/tests/committed_hbdef_decodes.rs` is that rule
    // made checkable from inside this crate - it decodes frozen `.hbdef` bytes
    // and re-encodes them byte for byte, so an INSERTED variant fails there
    // rather than in a silently different declaration six crates away.
    //
    // WHAT WAS BROKEN WITHOUT THEM. `TypeShape` was documented as having
    // TypeScript semantics and could not spell TypeScript's two most ordinary
    // type forms. MEASURED, before these variants existed:
    //
    //     interface ButtonProps { tone?: "primary" | "danger" }
    //       -> Err("`tone`: a union of 2 types is not modelled - the semantic
    //               AST has no sum type ...")
    //     interface Row { kind: "handle" }
    //       -> Named("unknown")
    //
    // The first is an outright refusal of ordinary source; the second is the
    // worse half, because it PARSES - `"handle"` reached `type_shape`'s `_ =>`
    // arm and became the same `Named("unknown")` every other unmodelled form
    // becomes, so a discriminant a record actually keys on arrived downstream
    // indistinguishable from a typo.
    //
    // THE REFUSAL WAS RIGHT WHILE IT STOOD, and that is why it is replaced
    // rather than relaxed. `union_shape`'s doc argues at length that collapsing
    // `Id | Blank` to `Id` is worse than refusing it, and it is: the author
    // declared a sum type and every reader saw one arm of it. The fix for "the
    // vocabulary has no sum type" is a sum type, not a looser collapse.
    /// **A TypeScript LITERAL TYPE** - `"primary"`, `42`, `true`: the type
    /// whose only inhabitant is that one value.
    ///
    /// # Why the payload is [`LiteralValue`] and not a literal enum of its own
    ///
    /// Because a second one would be a second width vocabulary, and
    /// [`LiteralValue`]'s own doc records that having two was the defect: it
    /// spells widths Rust's way, [`TypeShape`] spells them WIT's way, and the
    /// translation between them is written ONCE as
    /// [`type_shape`](LiteralValue::type_shape) and
    /// [`narrow`](LiteralValue::narrow) *"so no consumer maps a width by
    /// hand."* A literal type is a literal standing in type position - it is
    /// the same value, read in the other position - so it takes the same
    /// carrier, and `type_shape` is already exactly the widening a consumer
    /// wants from it (`Literal(String("primary"))` widens to `String`).
    ///
    /// The cost, stated: [`LiteralValue`] now reaches disk through two
    /// variants instead of one. It was already append-last for that reason and
    /// its doc already says so; this adds a second reader of the same rule, not
    /// a new rule.
    ///
    /// # Number and boolean are the same feature, not extra scope
    ///
    /// TypeScript's literal types are exactly string, number, boolean and
    /// bigint. Accepting `"a"` and refusing `1` would need its own refusal arm
    /// - MORE code than accepting, to ship a half of a form authors write whole
    /// (`type Step = -1 | 0 | 1`). Bigint is the one left out, and deliberately:
    /// [`LiteralValue`] has no bigint carrier, and minting one at the type level
    /// alone would create a literal the VALUE level cannot hold. `parse::literal_shape`
    /// refuses it by name instead.
    ///
    /// # Reduction
    ///
    /// WIT has no literal type, so the deterministic one-way reduction this
    /// enum's header describes is to the literal's own width - what
    /// [`LiteralValue::type_shape`] returns. The literal survives in the
    /// AUTHORED form, which is where a property sheet reads it from.
    Literal(LiteralValue),
    /// **A union** - `"primary" | "danger"`, `Id | Blank`.
    ///
    /// # This does NOT subsume [`TypeShape::Option`], and that is a decision
    ///
    /// `Option` is documented as `T | undefined` and is the one union this
    /// vocabulary had; `undefined` has no [`TypeShape`] spelling of its own, so
    /// `Option(T)` cannot be re-expressed as `Union([T, Undefined])` without a
    /// third variant for the absence - and absence-as-modelling is exactly what
    /// `Option` is for ([`LiteralValue`]'s doc settles the same question for
    /// the value level: there is no `Null` there because absence belongs to the
    /// type vocabulary). So the parser keeps partitioning the nullish members
    /// out and the two compose: `A | B | undefined` lowers to
    /// `Option(Union([A, B]))`, with `Option` outermost.
    ///
    /// # Normalized by the producer, not by the type
    ///
    /// `parse::union_shape` never builds a union of fewer than two members: one
    /// member is that member, and zero members with a nullish is
    /// `Named("unknown")`. A shorter `Vec` is still REPRESENTABLE here, because
    /// a `Vec` cannot say otherwise and a hand-built node may hold one; the
    /// emit spells an empty union `never` and a one-member union as that
    /// member, so nothing downstream has to guess. Neither shape round-trips
    /// through a re-parse, and neither is produced by one.
    ///
    /// # Reduction
    ///
    /// A union of string literals reduces to a WIT `enum`; the general case to
    /// a WIT `variant`. Both are reductions performed at evaluation, like every
    /// other operator here - see the enum header on why an unreduced node must
    /// never reach the edge.
    ///
    /// # Member order is the AUTHOR'S
    ///
    /// `"a" | "b"` and `"b" | "a"` are the same TypeScript type and two values
    /// here, deliberately: this crate's product describes authored SOURCE, and
    /// the emit is the identity on what was read. A consumer that needs a
    /// canonical order sorts at the point it needs one - which is the same
    /// place that decides whether the order is part of the shape's identity.
    Union(Vec<TypeShape>),
}

/// **The TypeScript spelling of an indexed access**, given an ALREADY-RENDERED
/// base - `Person["handle"]`.
///
/// Beside [`key_operator_spelling`] and for its reason: a dozen surfaces render
/// a `TypeShape` as text and each renders the BASE its own way, but none has a
/// reason to spell the BRACKETS differently, so the quoting cannot come out one
/// way here and another way there.
pub fn indexed_access_spelling(base: &str, key: &str) -> String {
    format!("{base}[\"{key}\"]")
}

/// **The TypeScript spelling of a key operator**, given an ALREADY-RENDERED
/// base - `Omit<Props, "a" | "b">`.
///
/// One function because there are a dozen places that render a `TypeShape` as
/// text (a WIT name, a drawn label, a change-detection token, a canonical
/// identity string) and each renders the BASE its own way, but none of them has
/// a reason to spell the KEYS differently. Every such site calls this with its
/// own base rendering, so a key list cannot come out quoted in one surface and
/// bare in another.
///
/// An empty key list spells `never`, TypeScript's own name for it, because
/// `Omit<Props, >` reparses as nothing.
pub fn key_operator_spelling(operator: &str, base: &str, keys: &[String]) -> String {
    let keys: Vec<String> = keys.iter().map(|key| format!("\"{key}\"")).collect();
    let keys = if keys.is_empty() { "never".to_string() } else { keys.join(" | ") };
    format!("{operator}<{base}, {keys}>")
}

/// **One literal, spelled as TypeScript source** - shared by VALUE position
/// (`emit_binding_expr`), TYPE position (`emit_type_shape`, for
/// [`TypeShape::Literal`]) and by every surface downstream that renders a
/// literal type as text.
///
/// Beside [`indexed_access_spelling`] and [`key_operator_spelling`] and for
/// their reason, which the literal type made load-bearing: `TypeShape::Literal`
/// put a [`LiteralValue`] in type position, so a dozen surfaces that already
/// render a `TypeShape` - a WIT name, a drawn label, a change-detection token,
/// a canonical identity string, a mock value - each acquired a literal to spell,
/// and none of them has a reason to quote, escape or round it differently. One
/// function, so `"back\slash"` cannot come out escaped in the emit and bare in
/// a token that is supposed to be an identity.
///
/// **The one place the two positions differ is a `Float64` that happens to be
/// whole.** [`float_literal_spelling`] writes `1.0`, because in value position a
/// bare `1` re-parses as `Int64` and the round trip is a law there. In type
/// position `1.0` is legal TypeScript and re-parses as `Literal(Int64(1))`, so
/// that one shape does not round trip - and it is not producible by a parse
/// either, because `numeric_literal` reads `1` as `Int64`. A hand-built
/// `Literal(Float64(1.0))` is the only way to reach it.
pub fn literal_spelling(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Bool(value) => if *value { "true" } else { "false" }.to_string(),
        LiteralValue::Int32(value) => value.to_string(),
        LiteralValue::Int64(value) => value.to_string(),
        LiteralValue::Float32(value) => float_literal_spelling(f64::from(*value)),
        LiteralValue::Float64(value) => float_literal_spelling(*value),
        LiteralValue::String(value) => string_literal_spelling(value),
    }
}

/// A float, with `.0` appended only when the rendered text is a bare integer -
/// anything with a point, an exponent or a non-finite spelling already
/// re-parses as what it is (or, for a non-finite, is not a TypeScript numeric
/// literal at all and could not have come from a parse).
fn float_literal_spelling(value: f64) -> String {
    let text = value.to_string();
    let integral =
        text.strip_prefix('-').unwrap_or(&text).bytes().all(|b| b.is_ascii_digit());
    if integral { format!("{text}.0") } else { text }
}

/// **A double-quoted TypeScript string literal for `value`, escaped.**
///
/// The unescaped spelling this replaces pushed `"`, the value, `"`, and failed
/// three ways (libhbui's `codec_round_trip.rs`, F3): a quote ended the literal
/// early, a newline left it unterminated, and a BACKSLASH was silently eaten -
/// `back\slash` emitted as `"back\slash"`, which re-parses as `backslash`. The
/// last is the one that matters, because both of the others are syntax errors a
/// re-parse reports and that one is a different string nothing complains about.
///
/// **Minimal, deliberately.** Only what changes meaning is escaped, so an
/// ordinary string is written the way an author would write it and the round
/// trip is not "escape everything" wearing a fix's clothes - the single quote,
/// the `$`, the backtick and every printable non-ASCII character go through
/// untouched. `U+2028`/`U+2029` are in the list because they are line
/// terminators to a JavaScript lexer even though they look like nothing.
pub fn string_literal_spelling(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other if (other as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", other as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// **The TypeScript spelling of a union**, given ALREADY-RENDERED members -
/// `"primary" | "danger"`.
///
/// The third of this trio and for the same reason: the surfaces that render a
/// `TypeShape` as text each render a MEMBER their own way - one spells a WIT
/// name, one a drawn label, one a canonical identity - but none has a reason to
/// spell the SEPARATOR differently, and a union whose bar came out ` | ` in one
/// surface and `|` in another is two identities for one type.
///
/// An empty member list spells `never`, TypeScript's own name for the empty
/// union, exactly as [`key_operator_spelling`] spells an empty key list. See
/// [`TypeShape::Union`] on why the degenerate lengths are spelled rather than
/// asserted against.
pub fn union_spelling(members: &[String]) -> String {
    if members.is_empty() { "never".to_string() } else { members.join(" | ") }
}

/// **The member a union READS AS to a consumer whose vocabulary has no sum
/// type**, or `None` when its members do not agree.
///
/// Nearly every consumer downstream projects a `TypeShape` into a vocabulary
/// that is SMALLER than TypeScript's - five Genius column types, GraphQL's
/// scalars, postgres's, a keyboard mode - and none of them has a sum type to
/// project a union into. Each one then faces the same question, which is NOT
/// "what is a union" but "does this union still have ONE answer in my
/// vocabulary": `"draft" | "published"` is one `text` column, and
/// `1 | "auto"` is not one of anything.
///
/// One function because the answer has to be the same in two places that are
/// not allowed to disagree - `highbay_data_service` decides a column's pg TYPE
/// and, separately, how to render a VALUE bound into it, and a union read as
/// `text` by one and as `jsonb` by the other writes a quoted JSON string into a
/// text column. A fold written per site is a seam waiting to drift, and this is
/// the kind of drift nothing reports.
///
/// The MEMBER comes back rather than the reading, because a caller that needs
/// to recurse (the value renderer) needs a shape and a caller that needs the
/// reading already has `read`. Which member: the first, and its identity
/// matters only when `read` is coarser than equality on the members - all of
/// them read the same by construction, so any would do, and the first is the
/// author's own (see [`TypeShape::Union`] on member order).
pub fn union_reading<'a, T: PartialEq>(
    members: &'a [TypeShape],
    read: impl Fn(&TypeShape) -> T,
) -> Option<&'a TypeShape> {
    let first = members.first()?;
    let reading = read(first);
    members[1..].iter().all(|member| read(member) == reading).then_some(first)
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
            extends: Vec::new(),
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
                    AttrValue::BindingExpr(BindingExpr::Call {
                        namespace: "host:zork".into(),
                        name: "frobnicate".into(),
                        type_args: vec![TypeShape::Named("Sprocket".into())],
                        args: vec![BindingExpr::Literal(LiteralValue::String(
                            "sprocket".into(),
                        ))],
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
            extends: Vec::new(),
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
                        BindingExpr::Literal(LiteralValue::Bool(true)),
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
                    then_branch: vec![BlockStmt::Return(BindingExpr::Null)],
                    else_branch: vec![BlockStmt::Try {
                        body: vec![BlockStmt::Return(BindingExpr::Path(vec![
                            "saved".into(),
                            "error".into(),
                        ]))],
                        error_slot: "error".into(),
                        catch: vec![BlockStmt::Return(BindingExpr::Literal(
                            LiteralValue::String("failed".into()),
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

    /// **A literal is refused rather than rounded, at every width.**
    ///
    /// [`LiteralValue::narrow`] is the one place the two width vocabularies
    /// meet, so this is where the contract `lower_arg` documents is actually
    /// pinned: past 2^53 an f64 stops being the integer it was written as, and
    /// a rounded id is indistinguishable from an id once it has been written
    /// down.
    #[test]
    fn a_literal_that_does_not_fit_its_declared_width_is_refused_not_rounded() {
        const EXACT: i64 = 9_007_199_254_740_992;
        assert_eq!(
            LiteralValue::Int64(7).narrow(&TypeShape::S32),
            Some(LiteralValue::Int32(7))
        );
        assert_eq!(
            LiteralValue::Int64(i64::from(i32::MAX) + 1).narrow(&TypeShape::S32),
            None,
            "one past i32 is refused, not wrapped"
        );
        assert_eq!(
            LiteralValue::Int64(EXACT).narrow(&TypeShape::S64),
            Some(LiteralValue::Int64(EXACT))
        );
        assert_eq!(
            LiteralValue::Int64(EXACT + 1).narrow(&TypeShape::S64),
            None,
            "past 2^53 is refused, not rounded"
        );
        assert_eq!(
            LiteralValue::Float64(1.5).narrow(&TypeShape::S64),
            None,
            "a fractional value is not a whole one, and truncating it is the defect"
        );
        assert_eq!(
            LiteralValue::Float64(2.0).narrow(&TypeShape::S32),
            Some(LiteralValue::Int32(2)),
            "a whole float DOES fit a whole declaration - exactly, which is the test"
        );
        assert_eq!(
            LiteralValue::Int64(3).narrow(&TypeShape::F64),
            Some(LiteralValue::Float64(3.0)),
            "widening an integer into a float is the one direction that is free"
        );
        assert_eq!(
            LiteralValue::String("s".into()).narrow(&TypeShape::S32),
            None
        );
        assert_eq!(
            LiteralValue::Int64(1).narrow(&TypeShape::U32),
            None,
            "U32 has no authored spelling, so nothing can be declared as one"
        );
    }

    /// **Every literal knows the [`TypeShape`] it already is** - the up-hill
    /// half of the one mapping, and total so a width added on either side
    /// fails to compile rather than falling through.
    #[test]
    fn a_literal_names_its_own_type_shape() {
        for (literal, shape) in [
            (LiteralValue::Bool(true), TypeShape::Bool),
            (LiteralValue::Int32(1), TypeShape::S32),
            (LiteralValue::Int64(1), TypeShape::S64),
            (LiteralValue::Float32(1.0), TypeShape::F32),
            (LiteralValue::Float64(1.0), TypeShape::F64),
            (LiteralValue::String("s".into()), TypeShape::String),
        ] {
            assert_eq!(literal.type_shape(), shape);
            assert_eq!(
                literal.narrow(&shape),
                Some(literal.clone()),
                "narrowing to the width it already is must be the identity"
            );
        }
    }
}
