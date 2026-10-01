//! SeaLion query engine: parser, planner, execution, WAND, ranking,
//! snippets, spelling (spec §27–50).
//!
//! Milestone 03 delivers Boolean queries over the in-memory index plus the
//! reference correctness oracle (§26):
//!
//! - [`query`]: Boolean query AST over normalized terms.
//! - [`execute`]: indexed Boolean execution (sorted DocId sets).
//! - [`reference`]: intentionally slow scan-based oracle; optimized search
//!   must agree with it exactly (§41).

pub mod execute;
pub mod query;
pub mod reference;
