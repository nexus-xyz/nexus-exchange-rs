//! Official Rust SDK for the Nexus Exchange API.
//!
//! A thin, idiomatic wrapper over the REST + WebSocket API. This is the crate
//! skeleton; REST endpoints ([`rest`]), authentication ([`auth`]), and
//! streaming ([`ws`]) are filled in incrementally.
#![deny(unreachable_pub)]
#![warn(missing_docs)]
#![warn(missing_debug_implementations)]

mod client;
mod config;
mod error;
mod ratelimit;

pub mod auth;
pub mod markets;
pub mod rest;
pub mod types;
pub mod ws;

pub use client::Client;
pub use config::{Config, Network, RateLimit};
pub use error::Error;
pub use markets::{OrderError, Rounding};

/// Convenience `Result` using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
