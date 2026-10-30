//! Two-way sync over the relay (design: `.workspace/docs/03-design/two-way-sync.md`).

// ---- sync-client ----
pub mod crypto;
pub mod engine;
pub mod pair;
pub mod transport;
#[cfg(test)]
mod test_relay;

// sync-client: contract stand-ins until sync-core merges (delete these two
// lines and `contract_stub.rs` at merge; core declares `pub mod op; pub mod store;`).
mod contract_stub;
pub use contract_stub::{op, store};
