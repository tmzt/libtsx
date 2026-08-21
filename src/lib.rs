//! libtsx — the single oxc boundary for the Highbay/Nocap stack.
//!
//! All `oxc_*` dependencies live only inside this crate; the public API is
//! plain owned Rust data:
//!
//! * [`dag`] — the serializable code-graph contract: the **element tree**
//!   ([`Element`], [`Node`], [`AttrValue`], [`TsxDocument`]), the
//!   **[`Definition`]** one screen or widget source declares (one exposed
//!   element, its stored symbol, and the interfaces/imports/handlers
//!   alongside), TS interface declarations, import edges and simple
//!   event-handler ops. Always available, serde-only, zero parser
//!   dependencies. Consumed by `nocap-witgen`, `highbay-build` and `libhbui`'s
//!   postcard codec.
//!   Imported calls ([`ImportedCall`]) live here too: an
//!   `onGrommet={frobnicate("sprocket")}` is graph data like everything else.
//!   So does [`ParserHost`], the provider an embedding hands the parse - the
//!   interface that keeps every name in an embedding's model out of this crate
//!   (LIBHBUI_PLAN Rule 52).
//! * [`template`] — the `{{ }}` placeholder scan ([`substitute`],
//!   [`placeholders`]). It is here because [`Node::Text`] carries the
//!   placeholders verbatim and says so, and because the two crates that read
//!   them are siblings over this one — see the module doc.
//! * [`parse_tsx`] / [`ParseCtx`] / [`extract_interfaces`] (feature
//!   `parse`, on by default) - the TSX parser built on oxc, which *produces*
//!   [`dag`] values and owns no
//!   types of its own. Downstream crates that only need the graph types can
//!   depend with `default-features = false` and skip the parser stack
//!   entirely — including the element tree, which is graph data and so is
//!   nameable without the parser.
//!
//! # The expression seam is a pair of conversions
//!
//! One [`dag::BindingExpr`] is the root of an expression subtree and needs no
//! document, so the two directions across the TEXT boundary are spelled as
//! Rust's own conversion traits rather than a bespoke vocabulary:
//!
//! * `impl From<&BindingExpr> for String` ([`emit`]) — **total**, so `From`.
//!   Not feature-gated: emitting names no `oxc_*` type.
//! * `impl TryFrom<&str> for BindingExpr` (feature `parse`) — **fallible**, so
//!   `TryFrom`, with [`ParseError`] saying which of the four refusals it was.
//!   It takes TEXT and not an `oxc_ast::Expression` because the quarantine
//!   above forbids naming one in a public signature; the parse happens inside.
//!
//! The two are **inverse over the image of the parse**, which is the law
//! `libhbui`'s `codec_round_trip.rs` measures over every authored `.tsx` in the
//! repository and over an exhaustive operand/position matrix.
//!
//! **The ladder continues upward in the crate that owns the next rung**, and
//! the orphan rule works out at each one without anything moving crates: a
//! crate may write the impl whose NEW type is its own, because `&T` is
//! fundamental (so `&BindingExpr` counts as local here, which is what makes
//! `From<&BindingExpr> for String` legal even though `String` is std's). The
//! instinct to put both directions beside the OLDER type is the one that does
//! not compile. A rung whose owner is a third crate — one that owns neither
//! end — needs a newtype or a free function; there is no impl for it.

pub mod dag;

pub mod emit;

pub mod template;

#[cfg(feature = "parse")]
mod parse;

#[cfg(feature = "parse")]
mod transpile;

// The element tree is `dag`'s, not `parse`'s — re-exported at the crate root
// (where callers have always found it) with no feature gate. `Definition` is
// re-exported beside it for the same reason: it is the shape a whole screen or
// widget source has, and naming it must not require the parser.
pub use dag::{
    AttrValue, DefError, Definition, EffectError, Element, ImportedCall, Node, ParserHost,
    Resolution, TsxDocument,
};

pub use emit::emit_tsx_document;

// The `{{ }}` scan, at the crate root beside the tree it scans - a caller that
// can name `Node::Text` can name what reads its placeholders.
pub use template::{placeholders, substitute};

#[cfg(feature = "parse")]
pub use parse::{
    ParseCtx, ParseCtxBuilder, ParseError, extract_interfaces, parse_app, parse_tsx,
};

#[cfg(feature = "parse")]
pub use transpile::transpile_ts;
