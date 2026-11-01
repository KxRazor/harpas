//! Benchmarks for the M1 risk prototypes
//!
//! Each example answers one *(measure)* question from the design documents and prints its results as Markdown
//! tables, ready to be recorded in the document. Run them in release mode:
//!
//! ```sh
//! cargo run --release -p harpas-bench --example shared-encoding
//! ```

pub mod chunk;
pub mod measure;
