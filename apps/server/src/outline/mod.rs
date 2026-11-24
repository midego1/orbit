//! Outline import: an Outline workspace (cloud or self-hosted) becomes Docs pages, from an
//! uploaded export (JSON or Markdown `.zip`) or through Outline's API.
//!
//! - [`model`]: typed shapes of Outline's ProseMirror documents, exports and API answers.
//! - [`archive`]: safe reading of an uploaded export and its scan.
//! - [`client`]: the API client (pagination, rate limits, attachments).
//! - [`convert`]: ProseMirror → Orbit blocks.
//! - [`import`]: the import jobs, their state and the API service.

pub mod archive;
pub mod client;
pub mod convert;
pub mod import;
pub mod model;
