mod model;
mod rule;

pub use model::{Slate, SlateId};
pub use service::{all, by_id, by_week, games, refresh};

pub(crate) use model::SlateProposal;
pub(crate) use service::counts;
#[cfg(test)]
pub(crate) use service::populate_at;

mod service;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
