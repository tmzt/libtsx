//! libtsx — the single oxc boundary for the Highbay/Nocap stack.
//!
//! All `oxc_*` dependencies live only inside this crate; the public API is
//! plain owned Rust data:
//!
//! * [`dag`] — the serializable `DagNode` code-graph contract (TS interface
//!   declarations + simple event-handler ops). Always available, serde-only,
//!   zero parser dependencies. Consumed by `nocap-witgen` and `highbay-build`.
//! * [`parse_tsx`] / [`extract_interfaces`] (feature `parse`, on by default) —
//!   the TSX parser built on oxc, plus TS `interface` extraction into the owned
//!   [`dag`] types. Downstream crates that only need the `DagNode` types can
//!   depend with `default-features = false` and skip the parser stack entirely.

pub mod dag;

#[cfg(feature = "parse")]
mod parse;

#[cfg(feature = "parse")]
pub use parse::{AttrValue, Element, Node, TsxDocument, extract_interfaces, parse_app, parse_tsx};
