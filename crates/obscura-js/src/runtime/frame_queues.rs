#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions};
use obscura_dom::{DomTree, NodeId};

use crate::import_map::ImportMap;
use crate::module_loader::{ModuleLoadActivity, ObscuraModuleLoader};
#[cfg(all(test, feature = "render"))]
use crate::ops::ensure_prepared_render;
use crate::ops::{build_extension, node_is_script, ObscuraState, StoredNetworkResponseBody};
#[cfg(feature = "render")]
use crate::ops::{
    begin_animation_task, clamp_scroll_offset, document_base_url, ensure_resolved_scroll,
};

use super::*;

impl ObscuraJsRuntime {


    /// Frame documents fetched by any realm that still need one of their own.
    /// The op queues onto the page's state whichever frame asked, so a frame
    /// nested inside a frame is drained here too.
    pub fn take_pending_frames(&self) -> Vec<crate::ops::PendingFrame> {
        let mut state = self.state.borrow_mut();
        state.pending_frame_bytes = 0;
        std::mem::take(&mut state.pending_frames)
    }

    /// postMessage traffic waiting to be delivered to another realm.
    pub fn take_pending_frame_messages(&self) -> Vec<crate::ops::PendingFrameMessage> {
        let mut state = self.state.borrow_mut();
        state.pending_frame_message_bytes = 0;
        std::mem::take(&mut state.pending_frame_messages)
    }

    /// Restore the configured V8 heap limit after the emergency headroom has
    /// allowed a terminated allocation to unwind. The callback is then armed
    /// again so a second hostile script cannot grow the isolate without bound.
    pub(crate) fn recover_heap_limit(&mut self) -> bool {
        if !self
            .heap_limit_state
            .tripped
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }

        self.runtime.v8_isolate().cancel_terminate_execution();
        let restore_limit = self
            .heap_limit_state
            .restore_limit
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        self.runtime
            .remove_near_heap_limit_callback(restore_limit);
        install_heap_limit_guard(
            &mut self.runtime,
            self.isolate_handle.clone(),
            self.heap_limit_state.clone(),
        );
        tracing::warn!("V8 heap limit reached: terminated the current JavaScript task");
        true
    }

    pub(crate) fn finish_heap_checked<T>(&mut self, result: Result<T, String>) -> Result<T, String> {
        if self.recover_heap_limit() {
            Err("JavaScript heap limit exceeded; execution terminated".to_string())
        } else {
            result
        }
    }

}
