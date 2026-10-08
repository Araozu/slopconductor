//! Transport-independent domain layer for Slop Conductor.
//!
//! Session identities, task transitions, budgets, and ownership rules belong
//! here. This bootstrap crate deliberately has no networking, storage, model
//! provider, or UI dependencies. Domain behavior will arrive in milestone M1.

pub mod provider;
