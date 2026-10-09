mod error;
mod migration;
mod model;
mod store;

pub use error::SalaryError;
pub use model::Salary;
pub(crate) use store::ManagedStore;
