//! Shared, named slates: public metadata types and the pure default
//! membership policy. Storage-backed lookups land in later tasks.

mod error;
mod model;
mod rule;

pub use error::SlateError;
pub use model::{Slate, SlateId};

pub(crate) use model::SlateProposal;
