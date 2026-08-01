//! libtsx — the single oxc boundary for the Highbay/Nocap stack.
//!
//! All `oxc_*` dependencies live only inside this crate; the public API is
//! plain owned Rust data:
//!
//! * [`dag`] — the serializable code-graph contract: the **element tree**
//!   ([`Element`], [`Node`], [`AttrValue`], [`TsxDocument`]), TS interface
//!   declarations, import edges and simple event-handler ops. Always
//!   available, serde-only, zero parser dependencies. Consumed by
//!   `nocap-witgen`, `highbay-build` and `libhbui`'s postcard codec.
//! * [`parse_tsx`] / [`extract_interfaces`] (feature `parse`, on by default) —
//!   the TSX parser built on oxc, which *produces* [`dag`] values and owns no
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
// (where callers have always found it) with no feature gate.
pub use dag::{AttrValue, Element, Node, TsxDocument};

#[cfg(feature = "parse")]
pub use parse::{extract_interfaces, parse_app, parse_tsx};

#[cfg(feature = "parse")]
pub use transpile::transpile_ts;
