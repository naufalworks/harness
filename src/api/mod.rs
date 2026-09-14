//! HTTP surface, split out of `main.rs` by P12-T01.
//!
//! Each submodule owns one cohesive concern so the routing table stays readable and the
//! behaviour of a single concern can be reviewed without reading the whole server.
pub(crate) mod assets;
pub(crate) mod auth;
pub(crate) mod error;
pub(crate) mod stream;
