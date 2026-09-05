use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions};
use obscura_dom::{DomTree, NodeId};

/// Re-exported so other crates (obscura-browser, obscura-cdp) can name the V8
/// isolate handle without taking a direct dependency on deno_core.
pub use deno_core::v8::IsolateHandle;

use crate::import_map::ImportMap;
use crate::module_loader::{ModuleLoadActivity, ObscuraModuleLoader};
#[cfg(all(test, feature = "render"))]
use crate::ops::ensure_prepared_render;
use crate::ops::{build_extension, node_is_script, ObscuraState, StoredNetworkResponseBody};
#[cfg(feature = "render")]
use crate::ops::{
    begin_animation_task, clamp_scroll_offset, document_base_url, ensure_resolved_scroll,
};

// Split from the former monolithic `runtime.rs` (16.5k lines). Aggregation only:
// semantic child modules below re-export everything, so `crate::runtime::*`
// and the public API paths are unchanged.

mod cdp_object;
mod event_loop;
mod frame_queues;
mod isolate;
mod module_pipeline;
mod page_config;
mod page_state;
mod script_exec;

pub use cdp_object::*;
pub use event_loop::*;
pub use frame_queues::*;
pub use isolate::*;
pub use module_pipeline::*;
pub use page_config::*;
pub use page_state::*;
pub use script_exec::*;

#[cfg(test)]
mod tests;
