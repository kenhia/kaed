//! kaed — an editor whose only user is an AI agent.
//!
//! Library surface exists for the binary and the integration tests; kaed is
//! not published as a general-purpose crate.

pub mod addr;
pub mod config;
pub mod deny;
pub mod dotenv;
pub mod errors;
pub mod fleet;
pub mod fsops;
pub mod history;
pub mod journal;
pub mod leak;
pub mod policy;
pub mod search;
pub mod secret_tool;
pub mod secrets;
pub mod server;
pub mod shapes;
pub mod txn;
pub mod version;
