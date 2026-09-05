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


    pub fn take_dom(&self) -> Option<DomTree> {
        let mut state = self.state.borrow_mut();
        #[cfg(feature = "render")]
        {
            state.prepared_render = None;
            state.pending_style_mutations.clear();
            state.render_resources = obscura_render::RenderResourceCache::default();
            state.stylesheet_cache = obscura_render::StylesheetCache::default();
            state.dynamic_fonts.clear();
            state.element_scroll_offsets.clear();
            state.resolved_scroll = None;
        }
        state.dom.take()
    }

    /// Export document-owned script preparation state before the runtime realm
    /// is temporarily destroyed.  Page suspension keeps the DOM alive, so the
    /// HTML "already started" flags must travel with it rather than resetting
    /// like window-global JavaScript state.
    pub fn started_script_ids(&self) -> Vec<u32> {
        let state = self.state.borrow();
        let mut ids = state
            .already_started_scripts
            .borrow()
            .iter()
            .map(|node_id| node_id.raw())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    /// Restore script preparation state only onto script nodes in the current
    /// DOM.  Callers use this exclusively for the same DomTree surviving a
    /// suspend/resume cycle; normal set_dom navigation starts from an empty set.
    pub fn restore_started_script_ids(&self, ids: &[u32]) {
        let state = self.state.borrow();
        let Some(dom) = state.dom.as_ref() else {
            return;
        };
        let valid = ids
            .iter()
            .copied()
            .map(NodeId::new)
            .filter(|node_id| node_is_script(dom, *node_id))
            .collect::<Vec<_>>();
        state.already_started_scripts.borrow_mut().extend(valid);
    }

    pub fn with_dom<R>(&self, f: impl FnOnce(&DomTree) -> R) -> Option<R> {
        let state = self.state.borrow();
        state.dom.as_ref().map(f)
    }

    /// Absolute URLs the page requested via fetch()/XHR, in request order
    /// (issue #301). Backs `--dump assets`.
    pub fn fetched_urls(&self) -> Vec<String> {
        self.state.borrow().fetched_urls.clone()
    }

    /// Drain the network events recorded for script-initiated requests
    /// (fetch/XHR/dynamic resource). The Page moves these into its own
    /// network_events so the CDP layer emits Network events for them (#406).
    pub fn take_js_network_events(&self) -> Vec<crate::ops::JsNetworkEvent> {
        std::mem::take(&mut self.state.borrow_mut().js_network_events)
    }

    pub fn dom_ref(&self) -> Option<std::cell::Ref<'_, Option<DomTree>>> {
        let r = self.state.borrow();
        if r.dom.is_some() {
            Some(std::cell::Ref::map(r, |s| &s.dom))
        } else {
            None
        }
    }

}
