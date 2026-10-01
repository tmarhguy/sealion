//! SeaLion query engine: parser, planner, execution, WAND, ranking,
//! snippets, spelling (spec §27–50).
//!
//! Milestone 03 delivers Boolean queries over the in-memory index plus the
//! reference correctness oracle (§26):
//!
//! - [`query`]: Boolean query AST over normalized terms.
//! - [`execute`]: indexed Boolean execution (sorted DocId sets).

pub mod execute;
pub mod query;
