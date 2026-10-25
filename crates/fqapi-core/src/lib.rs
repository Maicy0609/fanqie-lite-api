// fqapi_core - the Rust core for fanqie-lite-api.
//
// Ported from `fqapp/rust` (https://github.com/ch6vip/fqapp, GPL-3.0) at
// commit 4eaeac3. The business modules below are carried over unchanged; see
// docs/UPSTREAM.md for the exact deviation list. This file keeps its upstream
// shape (`//` comments, no inner crate docs) so it stays diffable against the
// original.
//
// Layering:
//   runtime   - process-wide core state: init/shutdown, cancellation, deadlines
//   dispatch  - the single request dispatcher (HTTP-shaped: Request -> Response)
//   endpoints - route table and business handlers
//   upstream  - signed HTTP access, device rotation, DH/CM key exchange
//   device    - persistent device pool + registration
//   sign      - request signing primitives

pub mod config;
pub mod core_log;
pub mod crypto;
pub mod device;
pub mod dispatch;
pub mod endpoints;
pub mod error;
pub mod filter;
pub mod json;
pub mod runtime;
pub mod server;
pub mod sign;
pub mod static_files;
pub mod timeutil;
pub mod upstream;
