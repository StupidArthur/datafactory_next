#![allow(unexpected_cfgs)]
#![allow(unused_features)]
#![cfg_attr(coverage, feature(coverage_attribute))]

mod compiler;
mod engine;
mod error;
mod format;
mod protocol;

pub use compiler::{compile_file, compile_source};
pub use engine::Engine;
pub use error::{EngineError, Result};
pub use format::{ProjectMetadata, Snapshot, TagDescriptor, Value};
pub use format::{ENGINE_ABI_VERSION, PROJECT_FORMAT_VERSION};
pub use protocol::{Request, Response};
