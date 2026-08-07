//! kaed — an editor whose only user is an AI agent.
//!
//! Library surface exists for the binary and the integration tests; kaed is
//! not published as a general-purpose crate.

pub mod addr;
pub mod config;
pub mod deny;
pub mod dotenv;
pub mod errors;
pub mod fsops;
pub mod history;
pub mod journal;
pub mod policy;
pub mod search;
pub mod secrets;
pub mod server;
pub mod txn;
pub mod version;
