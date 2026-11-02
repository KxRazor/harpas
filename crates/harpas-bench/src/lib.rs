//! Benchmarks for the M1 risk prototypes
//!
//! Each example answers one *(measure)* question from the design documents and prints its results as Markdown
//! tables, ready to be recorded in the document. Run them in release mode:
//!
//! ```sh
//! cargo run --release -p harpas-bench --example shared-encoding
//! cargo run --release -p harpas-bench --example jvm-bridge
//! ```
//!
//! The JVM prototype requires JDK 25 at runtime; see `examples/jvm-bridge/README.md`.

pub mod chunk;
pub mod measure;
