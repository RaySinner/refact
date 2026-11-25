mod registry;
mod store;
mod transport;
mod types;

#[cfg(test)]
mod tests;

pub use registry::ExternalParticipantRegistry;
pub(crate) use store::ExternalParticipantStore;
pub use transport::{ExternalConnection, ExternalParticipantTransport, UhpParticipantTransport};
pub use types::*;
