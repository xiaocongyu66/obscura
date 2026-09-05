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


pub(crate) const WATCHDOG_SCHEDULING_MARGIN_MS: u64 = 500;

impl ObscuraJsRuntime {
    /// Freeze the document timeline for one JavaScript task. Browser timelines
    /// update at task/rendering boundaries, not on each forced style or layout
    /// read. Keeping one sample across the task also lets repeated CSSOM reads
    /// share the retained layout on pages with running animations.
    pub(crate) fn begin_javascript_task(&mut self) {
        // Some internal callers intentionally ignore script errors. Recover a
        // heap-limit termination before any later task enters V8 even when the
        // caller that triggered it did not need the error value.
        self.recover_heap_limit();
        #[cfg(feature = "render")]
        begin_animation_task(&mut self.state.borrow_mut());
    }
    pub fn new() -> Self {
        Self::with_base_url("about:blank")
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_base_url_and_proxy(base_url, None)
    }

    /// Construct a runtime whose ES-module loader routes dynamic imports
    /// through `proxy_url` (#139). `None` is equivalent to `with_base_url`
    /// (direct connection).
    pub fn with_base_url_and_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        let state = Rc::new(RefCell::new(ObscuraState::new()));
        let state_clone = state.clone();
        let import_map = state.borrow().import_map.clone();

        let module_loader = ObscuraModuleLoader::with_page_state(
            base_url,
            proxy_url,
            &state,
            import_map.clone(),
        );
        let module_load_activity = module_loader.activity();
        let loaded_module_specifiers = module_loader.loaded_specifiers();
        let module_loader = Rc::new(module_loader);

        // Build the isolate under the process-wide creation lock so two
        // connection threads never construct isolates concurrently (#430).
        let (runtime, isolate_handle, heap_limit_state) = {
            let _create_guard = ISOLATE_CREATE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            let mut runtime = JsRuntime::new(RuntimeOptions {
                extensions: vec![build_extension()],
                module_loader: Some(module_loader),
                startup_snapshot: Some(SNAPSHOT),
                ..Default::default()
            });

            {
                let op_state = runtime.op_state();
                let mut op_state = op_state.borrow_mut();
                op_state.put(state_clone);
                // Empty until a frame realm exists, which is what keeps the
                // lookup free for pages that have no frames.
                op_state.put(Rc::new(RefCell::new(crate::ops::RealmStates::default())));
            }

            let isolate_handle = runtime.v8_isolate().thread_safe_handle();
            let heap_limit_state = std::sync::Arc::new(HeapLimitState::default());
            install_heap_limit_guard(
                &mut runtime,
                isolate_handle.clone(),
                heap_limit_state.clone(),
            );

            runtime
                .execute_script(
                    "<obscura:init>",
                    "globalThis.__obscura_objects = {}; globalThis.__obscura_oid = 0;".to_string(),
                )
                .expect("init should not fail");

            (runtime, isolate_handle, heap_limit_state)
        };

        let mut instance = ObscuraJsRuntime {
            runtime,
            state,
            object_store: HashMap::new(),
            object_counter: 0,
            import_map,
            module_load_activity,
            isolate_handle,
            heap_limit_state,
            module_evaluations: HashMap::new(),
            loaded_module_specifiers,
            evaluated_module_specifiers: HashMap::new(),
            ops_handoff: None,
        };
        // Take the op table before any page script can run, and drop the global
        // that exposed it in the same step.
        instance.ops_handoff = instance.take_ops_handoff();
        instance
    }

    /// Creates an additional realm in this isolate: a second `v8::Context`.
    ///
    /// The startup snapshot already contains the whole bootstrap (see
    /// `build.rs`), so a context restored from it arrives with every DOM class
    /// and shim installed. Building a realm is therefore a context restore, not
    /// a re-parse of the whole bootstrap.
    ///
    /// The new context has no ops: deno_core binds those into the main context
    /// only. Use [`Self::share_ops_with_realm`] to give it the same `Deno.core`
    /// object, which is legal because native function objects are shareable
    /// between contexts of one isolate.
    pub(crate) fn create_realm_context(
        &mut self,
    ) -> Option<deno_core::v8::Global<deno_core::v8::Context>> {
        let context = {
            let isolate = self.runtime.v8_isolate();
            let scope = &mut deno_core::v8::HandleScope::new(isolate);
            let context = deno_core::v8::Context::from_snapshot(
                scope,
                1,
                deno_core::v8::ContextOptions::default(),
            )
            .or_else(|| {
                deno_core::v8::Context::from_snapshot(
                    scope,
                    0,
                    deno_core::v8::ContextOptions::default(),
                )
            })?;
            deno_core::v8::Global::new(scope, context)
        };
        Some(context)
    }

    /// Takes the ops object bootstrap handed out, and removes the handoff from
    /// the global so page script can never reach `Deno.core.ops`.
    ///
    /// deno_core hides `globalThis.Deno` after setup and bootstrap keeps its
    /// reference in a private const, so this handoff is the only way for the
    /// host to reach the bound op functions and pass them to a child realm.
    pub(crate) fn take_ops_handoff(&mut self) -> Option<deno_core::v8::Global<deno_core::v8::Value>> {
        use deno_core::v8;

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Local::new(scope, main);
        let scope = &mut v8::ContextScope::new(scope, context);

        let handoff_key = v8::String::new(scope, "__obscura_core_handoff")?;
        let ops_key = v8::String::new(scope, "ops")?;
        let global = context.global(scope);

        let core = global.get(scope, handoff_key.into())?;
        let core = core.to_object(scope)?;
        let ops = core.get(scope, ops_key.into())?;
        if !ops.is_object() {
            return None;
        }
        let ops = v8::Global::new(scope, ops);
        global.delete(scope, handoff_key.into());
        Some(ops)
    }

    /// Points a child realm's `Deno.core.ops` at the main realm's ops object.
    ///
    /// A realm restored from the snapshot has its own `Deno.core` with an empty
    /// ops table, and its bootstrap captured that exact object, so filling the
    /// `ops` table on it is enough to give every shim in that realm a working
    /// op surface. The functions are shared, not copied: same isolate.
    pub(crate) fn share_ops_with_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) -> bool {
        use deno_core::v8;

        let Some(ops) = self.ops_handoff.clone() else {
            return false;
        };
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Local::new(scope, realm);
        let scope = &mut v8::ContextScope::new(scope, context);

        let Some(handoff_key) = v8::String::new(scope, "__obscura_core_handoff") else {
            return false;
        };
        let Some(ops_key) = v8::String::new(scope, "ops") else {
            return false;
        };
        let global = context.global(scope);
        let Some(core) = global.get(scope, handoff_key.into()) else {
            return false;
        };
        let Some(core) = core.to_object(scope) else {
            return false;
        };
        // `Deno.core.ops` is non-writable and non-configurable, so the table
        // cannot be swapped wholesale: V8 reports success and changes nothing.
        // Copy the bound op functions into the realm's existing table instead.
        let Some(target) = core
            .get(scope, ops_key.into())
            .and_then(|value| value.to_object(scope))
        else {
            return false;
        };
        let source = v8::Local::new(scope, ops);
        let Some(source) = source.to_object(scope) else {
            return false;
        };
        let Some(names) = source.get_own_property_names(scope, Default::default()) else {
            return false;
        };
        let mut copied = 0;
        for index in 0..names.length() {
            let Some(key) = names.get_index(scope, index) else {
                continue;
            };
            let Some(value) = source.get(scope, key) else {
                continue;
            };
            if target.set(scope, key, value).unwrap_or(false) {
                copied += 1;
            }
        }
        // The child realm must not expose the handoff to frame script either.
        global.delete(scope, handoff_key.into());
        copied > 0
    }

    /// Copies the identity-critical global constructors from the main realm
    /// into a child realm. obscura's object model is single-realm: DOM
    /// wrappers live in the main realm and shared code throws main-realm
    /// exceptions, so a child realm keeping its own `DOMException`/`TypeError`
    /// fails every `instanceof` check the WPT three-argument
    /// assert_throws_dom/js form makes against `(doc.defaultView||self)`.
    /// Runs after `share_ops_with_realm` and before `__obscura_init`, so the
    /// frame's own bootstrap goes on to use the shared constructors too.
    pub(crate) fn share_global_constructors_with_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) -> usize {
        use deno_core::v8;

                // Servo 语义(见 components/script/realms.rs 的 DomTypeHolder):每个
        // realm 各自持有完整构造器集,跨 realm 断言(WPT win.XXX)以目标 realm
        // 为准 —— realm 自洽即通过。把主 realm 的类覆盖到 iframe 的 window 上
        // 反而制造两个类并存(iframe 的 DOM 实现仍用自己 bootstrap 的类建对象,
        // window.Element 却指向主类 → instanceof 断裂)。
        const NAMES: &[&str] = &[];
        let main_global = self.runtime.main_context().clone();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let main = v8::Local::new(scope, main_global);
        let realm_context = v8::Local::new(scope, realm);

        let mut copied = 0;
        for name in NAMES {
            let Some(key) = v8::String::new(scope, name) else { continue };
            let source = {
                let scope = &mut v8::ContextScope::new(scope, main);
                main.global(scope).get(scope, key.into())
            };
            let Some(source) = source else { continue };
            let ok = {
                let scope = &mut v8::ContextScope::new(scope, realm_context);
                realm_context
                    .global(scope)
                    .set(scope, key.into(), source)
                    .unwrap_or(false)
            };
            if ok {
                copied += 1;
            }
        }
        copied
    }

    /// Runs `source` inside `realm` and returns its value as a string. Errors
    /// come back as `Err(message)`.
    pub(crate) fn eval_in_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
        source: &str,
    ) -> Result<String, String> {
        use deno_core::v8;

        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Local::new(scope, realm);
        let scope = &mut v8::ContextScope::new(scope, context);
        let scope = &mut v8::TryCatch::new(scope);

        let code = v8::String::new(scope, source).ok_or("source too large")?;
        let script = match v8::Script::compile(scope, code, None) {
            Some(script) => script,
            None => return Err(exception_text(scope)),
        };
        match script.run(scope) {
            Some(value) => Ok(value.to_rust_string_lossy(scope)),
            None => Err(exception_text(scope)),
        }
    }

    /// Copies the browser-identity globals from the main realm into `realm`.
    ///
    /// A frame must present the same identity as its parent: anti-bot code
    /// fingerprints inside the frame and compares it with the top document.
    /// Copying the values the parent already has makes that true by
    /// construction, instead of relying on a caller to reapply the same
    /// settings to both.
    pub(crate) fn copy_identity_to_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) {
        use deno_core::v8;

        const IDENTITY_GLOBALS: [&str; 7] = [
            "__obscura_ua",
            "__obscura_platform",
            "__obscura_ua_platform",
            "__obscura_ua_platform_version",
            "__obscura_stealth",
            "__obscura_geo_lat",
            "__obscura_geo_lon",
        ];

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);

        let main_context = v8::Local::new(scope, main);
        let mut carried = Vec::new();
        {
            let scope = &mut v8::ContextScope::new(scope, main_context);
            let global = main_context.global(scope);
            for name in IDENTITY_GLOBALS {
                let Some(key) = v8::String::new(scope, name) else {
                    continue;
                };
                match global.get(scope, key.into()) {
                    Some(value) if !value.is_undefined() => {
                        carried.push((name, v8::Global::new(scope, value)));
                    }
                    _ => {}
                }
            }
        }

        let realm_context = v8::Local::new(scope, realm);
        let scope = &mut v8::ContextScope::new(scope, realm_context);
        let global = realm_context.global(scope);
        for (name, value) in carried {
            let Some(key) = v8::String::new(scope, name) else {
                continue;
            };
            let value = v8::Local::new(scope, value);
            global.set(scope, key.into(), value);
        }
    }

    /// Gives a frame's state the resources the page owns: cookie jar, HTTP
    /// client, callbacks and the stealth transport. A frame shares these with
    /// its page, exactly as it shares them in a browser.
    pub(crate) fn share_resources_with(&self, frame: &mut ObscuraState) {
        let parent = self.state.borrow();
        frame.cookie_jar = parent.cookie_jar.clone();
        frame.http_client = parent.http_client.clone();
        frame.callbacks = parent.callbacks.clone();
        frame.encoding = parent.encoding.clone();
        frame.blocked_urls = parent.blocked_urls.clone();
        frame.intercept_enabled = parent.intercept_enabled;
        frame.page_in_flight = parent.page_in_flight.clone();
        #[cfg(feature = "stealth")]
        {
            frame.stealth_client = parent.stealth_client.clone();
        }
    }

    /// The origin of the document this runtime is running, or `"null"` for a
    /// scheme that has no tuple origin.
    pub(crate) fn page_origin(&self) -> String {
        let url = self.state.borrow().url.clone();
        match url::Url::parse(&url) {
            Ok(parsed) if parsed.origin().is_tuple() => parsed.origin().ascii_serialization(),
            _ => "null".to_string(),
        }
    }

    /// The full URL of the document this runtime is running. Used as the
    /// subresource-resolution base for about:blank / about:srcdoc child
    /// frames, which inherit it per HTML spec.
    pub(crate) fn page_url(&self) -> String {
        self.state.borrow().url.clone()
    }

    /// Gives a same-origin frame realm the page's security token.
    ///
    /// V8 access-checks property reads across contexts and answers `undefined`
    /// unless the two carry the same token, which is how a browser keeps one
    /// origin out of another's window. Two contexts of one origin must share a
    /// token, or the page reads its own frame's globals as undefined. Only
    /// ever called after an origin comparison; a cross-origin frame keeps its
    /// own token and stays opaque.
    pub(crate) fn share_security_token_with_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) {
        use deno_core::v8;

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let main = v8::Local::new(scope, main);
        let realm = v8::Local::new(scope, realm);
        let token = main.get_security_token(scope);
        realm.set_security_token(token);
    }

    /// Publishes a frame realm's own `window` and `document` objects into the
    /// page realm, under `__obscura_frameObjects[frameId]`.
    ///
    /// This is what the single isolate buys. Objects cannot cross isolates, so
    /// a parent could only ever be handed a copy or a shim; within one isolate
    /// it can hold the frame's real globals, which is what a browser gives it
    /// for a same-origin frame. `contentWindow.someGlobal` is then a plain
    /// property read of the frame's own object, and `contentDocument` is the
    /// document the frame's scripts actually mutated.
    pub(crate) fn publish_realm_objects(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
        frame_id: u32,
    ) -> bool {
        use deno_core::v8;

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);

        // Read the frame's globals first, then install them in the page realm.
        // Both contexts belong to this isolate, so the handles stay valid
        // across the switch.
        let realm_context = v8::Local::new(scope, realm);
        let (frame_window, frame_document) = {
            let scope = &mut v8::ContextScope::new(scope, realm_context);
            let global = realm_context.global(scope);
            let Some(key) = v8::String::new(scope, "document") else {
                return false;
            };
            let document = global.get(scope, key.into());
            (
                v8::Global::new(scope, global),
                document.map(|value| v8::Global::new(scope, value)),
            )
        };

        let main_context = v8::Local::new(scope, main);
        let scope = &mut v8::ContextScope::new(scope, main_context);
        let global = main_context.global(scope);
        let Some(registry_key) = v8::String::new(scope, "__obscura_frameObjects") else {
            return false;
        };
        let registry = match global
            .get(scope, registry_key.into())
            .and_then(|value| value.to_object(scope))
        {
            Some(registry) if !registry.is_null_or_undefined() => registry,
            _ => {
                let fresh = v8::Object::new(scope);
                global.set(scope, registry_key.into(), fresh.into());
                fresh
            }
        };

        let entry = v8::Object::new(scope);
        let window = v8::Local::new(scope, frame_window);
        if let Some(key) = v8::String::new(scope, "window") {
            entry.set(scope, key.into(), window.into());
        }
        if let (Some(key), Some(document)) = (
            v8::String::new(scope, "document"),
            frame_document.map(|document| v8::Local::new(scope, document)),
        ) {
            entry.set(scope, key.into(), document);
        }
        let index = v8::Integer::new_from_unsigned(scope, frame_id);
        registry.set(scope, index.into(), entry.into()).unwrap_or(false)
    }

    /// The table ops consult to find the calling realm's document.
    pub(crate) fn realm_states(&self) -> Rc<RefCell<crate::ops::RealmStates>> {
        self.runtime
            .op_state()
            .borrow()
            .borrow::<Rc<RefCell<crate::ops::RealmStates>>>()
            .clone()
    }
}
