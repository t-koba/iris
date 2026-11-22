//! Compatibility facade for dependents that still require the `paste` API.
//!
//! Iris carries no copy of the unmaintained `paste` implementation: these
//! macros come from the maintained `pastey` crate, re-exported under the
//! names those dependents expect.
//!
//! ```
//! paste::paste! {
//!     const IRIS_PASTE_SMOKE: u8 = 1;
//! }
//! paste::item! {
//!     const IRIS_ITEM_SMOKE: u8 = 2;
//! }
//! const IRIS_EXPR_BASE: u8 = 3;
//! let value = paste::expr! { [<IRIS_EXPR_ BASE>] };
//! assert_eq!((IRIS_PASTE_SMOKE, IRIS_ITEM_SMOKE, value), (1, 2, 3));
//! ```

pub use pastey::{expr, item, paste};
