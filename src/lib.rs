//! rx0: a fast, ultra-light code navigator served in the browser.
//!
//! Slice 1 ports the server shell: CLI, router, embedded static assets,
//! the path sandbox, and `/api/meta`. Later slices add index, search,
//! highlighting, LSP, agent dispatch, and the rest of `/api/*`.

pub mod agent;
pub mod calls;
pub mod fuzzy;
pub mod git;
pub mod highlight;
pub mod ignore;
pub mod index;
pub mod lsp;
pub mod lspnav;
pub mod lspservers;
pub mod lspsetup;
pub mod markdown;
pub mod metrics;
pub mod search;
pub mod server;
pub mod settings;
pub mod symbols;
pub mod telemetry;
pub mod update;

#[cfg(test)]
pub(crate) mod testutil;

/// Crate version, mirroring the Go `VERSION` file.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
