mod model;
mod rule;

pub use model::{Slate, SlateId, SlateStatus};
pub(crate) use service::{all, by_id, games, refresh};

pub(crate) use model::{SlateProposal, spans_multiple_days};
pub(crate) use service::counts;
#[cfg(test)]
pub(crate) use service::{by_week, populate_at};

mod service;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
