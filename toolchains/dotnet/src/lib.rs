pub mod config;
pub mod dotnet_install;
pub mod global_json;
pub mod infer_tasks;
pub mod msbuild;
pub mod nuget_lock;

#[cfg(feature = "wasm")]
mod tier1;
#[cfg(feature = "wasm")]
mod tier2;
#[cfg(feature = "wasm")]
mod tier3;

#[cfg(feature = "wasm")]
pub use tier1::*;
#[cfg(feature = "wasm")]
pub use tier2::*;
#[cfg(feature = "wasm")]
pub use tier3::*;
