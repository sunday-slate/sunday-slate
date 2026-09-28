//! Contest setup — the sitewide, admin-only feature for configuring which
//! contests exist. Nested under the admin router, which applies the `AdminUser`
//! gate for the whole subtree. The reusable contest domain lives in
//! `crate::contests`.

mod handlers;
mod slate;
pub mod store;

pub use handlers::router;
