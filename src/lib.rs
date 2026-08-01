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
//!   Effect bindings ([`NamedEffect`]) live here too: an
//!   `onGrommet={frobnicate("sprocket")}` is graph data like everything else.
//!   So does [`ParserHost`], the provider an embedding hands the parse - the
//!   interface that keeps every name in an embedding's model out of this crate
//!   (LIBHBUI_PLAN Rule 52).
//! * [`parse_tsx`] / [`ParseCtx`] / [`extract_interfaces`] (feature
//!   `parse`, on by default) - the TSX parser built on oxc, which *produces*
//!   [`dag`] values and owns no
//!   types of its own. Downstream crates that only need the graph types can
//!   depend with `default-features = false` and skip the parser stack
//!   entirely — including the element tree, which is graph data and so is
//!   nameable without the parser.

pub mod dag;

#[cfg(feature = "parse")]
mod parse;

#[cfg(feature = "parse")]
mod transpile;

// The element tree is `dag`'s, not `parse`'s — re-exported at the crate root
// (where callers have always found it) with no feature gate. `Definition` is
// re-exported beside it for the same reason: it is the shape a whole screen or
// widget source has, and naming it must not require the parser.
pub use dag::{
    AttrValue, DefError, Definition, EffectError, Element, NamedEffect, Node, ParserHost,
    Resolution, TsxDocument,
};

#[cfg(feature = "parse")]
pub use parse::{
    ParseCtx, ParseCtxBuilder, ParseError, extract_interfaces, parse_app, parse_tsx,
};

#[cfg(feature = "parse")]
pub use transpile::transpile_ts;
