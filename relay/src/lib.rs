//! Devdy Remote Control Relay — library crate.
//!
//! A WebSocket bridge (SRS-RC-C1, Unit U1) that routes opaque, E2E-encrypted
//! envelopes between a Host and a single Controller by `room_id`. The relay
//! never decrypts, stores, or logs payloads (SEC-006 / CON-04 / NFR-005) and
//! binds `localhost` behind a reverse proxy that terminates TLS (Topology B).
//!
//! Public surface is kept small on purpose: [`config::Config`] and
//! [`server::serve`] are enough to embed the relay (used by integration tests).

pub mod config;
pub mod protocol;
pub mod ratelimit;
pub mod room;
pub mod server;

pub use config::Config;
pub use server::{serve, RelayHandle};
