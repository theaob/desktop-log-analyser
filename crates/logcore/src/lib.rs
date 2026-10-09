//! Core engine of the desktop log analyser: discovers log files in a folder, parses log4j /
//! Logback / JSON / plain text logs into events, stores them in compressed chunks, and runs
//! LogQL-style queries with a level-stacked volume histogram.

pub mod bloom;
pub mod chunk;
pub mod discover;
pub mod export;
pub mod formats;
pub mod ingest;
pub mod labels;
pub mod log4j;
pub mod model;
pub mod parse;
pub mod query;
pub mod reader;
pub mod search;
pub mod store;
pub mod time;

pub use ingest::{open_or_build, preview, Progress};
pub use search::{search, SearchRequest, SearchResult};
pub use store::{FolderSettings, Workspace};
