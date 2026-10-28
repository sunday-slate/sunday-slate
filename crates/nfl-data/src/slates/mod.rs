mod error;
mod model;
mod rule;

pub use error::SlateError;
pub use model::{Slate, SlateId};
pub use service::{by_id, by_week, games};

pub(crate) use model::SlateProposal;

mod service;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use service::{slate_at, slate_games_at, slates_at};
