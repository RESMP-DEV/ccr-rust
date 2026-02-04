//! Transform module with factory-based registry.
//!
//! This module provides a factory-based approach to transformer instantiation,
//! separate from the existing static registry in `transformer.rs`.

pub mod maxtoken;
pub mod registry;

pub use registry::TransformerRegistry;
