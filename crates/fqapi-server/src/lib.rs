//! HTTP front end for `fqapi_core`.
//!
//! The core owns routing, signing, the device pool and decryption; this crate
//! owns the transport concerns a public listener needs and the loopback
//! adapter (`fqapi_core::server`) deliberately does not have: bearer
//! authentication, CORS, a concurrency gate, request deadlines and graceful
//! shutdown.
//!
//! Paths are unchanged from upstream (`/api/v1/*`), so an existing client only
//! has to swap its transport.

pub mod auth;
pub mod config;
pub mod health;
pub mod http;
pub mod poolkey;
