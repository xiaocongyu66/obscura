#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

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

use super::*;


#[cfg(feature = "render")]
pub(crate) struct RuntimeCanvasSurfaceSource<'a>(
    pub(crate) &'a HashMap<NodeId, crate::ops::CanvasBackingSurface>,
);

#[cfg(feature = "render")]
impl obscura_render::CanvasSurfaceSource for RuntimeCanvasSurfaceSource<'_> {
    fn surface(&self, node: NodeId) -> Option<obscura_render::CanvasSurface<'_>> {
        let surface = self.0.get(&node)?;
        obscura_render::CanvasSurface::from_rgba8(
            surface.width,
            surface.height,
            surface.pixels.as_ref(),
        )
    }
}

pub(crate) static SNAPSHOT: &[u8] = include_bytes!(env!("OBSCURA_SNAPSHOT_PATH"));

/// Serializes V8 isolate construction across OS threads. The thread-per-
/// connection server (issue #430) builds isolates on many threads. The main
/// thread already warms up V8 once before any connection thread starts (see the
/// `ObscuraJsRuntime::new` warmup in `obscura-cdp` server startup), which is
/// what actually prevents the `InitializeBuiltinJSDispatchTable` segfault of a
/// first isolate built off the main thread. This lock is defense-in-depth: it
/// keeps two connections from running V8's isolate setup concurrently in case
/// any residual first-time process init races. Construction is rare and fast, so
/// serializing it costs nothing measurable; isolate *execution* stays fully
/// parallel, each isolate on its own thread with no shared lock.
pub(crate) static ISOLATE_CREATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) const DEFAULT_CDP_AWAIT_TIMEOUT_MS: u64 = 30_000;
pub(crate) const HEAP_LIMIT_RECOVERY_HEADROOM_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct HeapLimitState {
    pub(crate) tripped: std::sync::atomic::AtomicBool,
    pub(crate) restore_limit: std::sync::atomic::AtomicUsize,
}

pub(crate) fn install_heap_limit_guard(
    runtime: &mut JsRuntime,
    isolate_handle: IsolateHandle,
    state: std::sync::Arc<HeapLimitState>,
) {
    runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
        let _ = state.restore_limit.compare_exchange(
            0,
            current_limit,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
        state
            .tripped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        isolate_handle.terminate_execution();
        current_limit.saturating_add(HEAP_LIMIT_RECOVERY_HEADROOM_BYTES)
    });
}

pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("unknown panic")
}

#[cfg(feature = "render")]
pub(crate) fn with_sync_render_loading_disabled<R>(
    state: &mut ObscuraState,
    capture: impl FnOnce(&mut ObscuraState) -> R,
) -> R {
    let previous = state
        .render_resources
        .set_sync_loading_enabled(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| capture(state)));
    state
        .render_resources
        .set_sync_loading_enabled(previous);
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[derive(Debug, Clone)]
pub struct RemoteObjectInfo {
    pub js_type: String,
    pub subtype: Option<String>,
    pub class_name: String,
    pub description: String,
    pub object_id: Option<String>,
    pub value: Option<serde_json::Value>,
}

pub struct ObscuraJsRuntime {
    pub(crate) runtime: JsRuntime,
    pub(crate) state: Rc<RefCell<ObscuraState>>,
    pub(crate) object_store: HashMap<String, String>,
    pub(crate) object_counter: u64,
    pub(crate) import_map: Rc<RefCell<ImportMap>>,
    /// Loader-owned signal for pending dynamic-import graph fetches. This is
    /// intentionally separate from page fetch/XHR activity so analytics does
    /// not hold screenshot readiness open.
    pub(crate) module_load_activity: std::sync::Arc<ModuleLoadActivity>,
    /// Thread-safe handle to this runtime's V8 isolate, captured at
    /// construction. Lets a watchdog be armed from `&self` (the CDP dispatcher
    /// only holds `&Page` on the hot path) and is stable for the isolate's life.
    pub(crate) isolate_handle: IsolateHandle,
    /// Signals that V8 approached its configured heap limit. The callback
    /// terminates the current script and temporarily raises the limit just
    /// enough for V8 to unwind instead of aborting the worker process.
    pub(crate) heap_limit_state: std::sync::Arc<HeapLimitState>,
    /// Browser module-map evaluation is idempotent. deno_core 0.350 asserts if
    /// the same ModuleId is evaluated twice, so retain the first outcome for
    /// duplicate script tags and roots already seen by Obscura.
    pub(crate) module_evaluations: HashMap<deno_core::ModuleId, Result<(), String>>,
    /// Append-only record owned by the module loader. A cursor around each
    /// graph load identifies the dependency specifiers that become evaluated
    /// with its root.
    pub(crate) loaded_module_specifiers: Rc<RefCell<Vec<String>>>,
    /// Successful graph evaluation also evaluates every dependency. Remember
    /// those URLs so a dependency later encountered as a top-level script is a
    /// browser-style no-op instead of a second deno_core `mod_evaluate` call.
    pub(crate) evaluated_module_specifiers: HashMap<String, Result<(), String>>,
    /// The bound op table, taken from bootstrap at construction and removed from
    /// the global in the same step. Child frame realms are handed this object so
    /// their shims can call ops; nothing else can reach it, including page
    /// script.
    pub(crate) ops_handoff: Option<deno_core::v8::Global<deno_core::v8::Value>>,
}

/// Renders a caught V8 exception as a message for realm evaluation errors.
pub(crate) fn exception_text(
    scope: &mut deno_core::v8::TryCatch<'_, deno_core::v8::HandleScope<'_>>,
) -> String {
    match scope.exception() {
        Some(exception) => exception.to_rust_string_lossy(scope),
        None => "unknown error".to_string(),
    }
}

/// A fetched and instantiated module graph whose evaluation is intentionally
/// delayed until the HTML script scheduler reaches its post-parse turn.
pub struct PreparedModule {
    pub(crate) module_id: deno_core::ModuleId,
    pub(crate) description: String,
    pub(crate) entry_specifier: Option<String>,
    pub(crate) graph_specifiers: Vec<String>,
}

pub(crate) fn remaining_deadline_ms(deadline: tokio::time::Instant) -> Option<u64> {
    let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
    if remaining.is_zero() {
        return None;
    }
    // Round up so a positive sub-millisecond remainder still gets one bounded
    // event-loop turn. The watchdog supplies the hard wall-clock boundary.
    let millis = remaining
        .as_millis()
        .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
    Some(millis.min(u128::from(u64::MAX)) as u64)
}

/// Handle to an armed V8 execution watchdog (see [`ObscuraJsRuntime::arm_watchdog`]).
/// Holds the cancel channel and the watchdog thread; pass it back to
/// `disarm_watchdog` to stop the watchdog and learn whether it fired.
pub struct WatchdogToken {
    pair: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    join: Option<std::thread::JoinHandle<()>>,
    fired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Arm a V8 termination watchdog directly from an isolate handle, with no
/// runtime borrow. The CDP dispatcher uses this to bound every command so a
/// hung page cannot hold this connection's V8 lock forever. Pair with
/// [`WatchdogToken::stop`]; if `stop` returns true, clear the termination flag
/// via [`ObscuraJsRuntime::cancel_termination`] before reusing the isolate.
pub fn spawn_watchdog(handle: IsolateHandle, budget: std::time::Duration) -> WatchdogToken {
    let pair = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pair_c = pair.clone();
    let fired_c = fired.clone();
    let join = std::thread::spawn(move || {
        let (lock, cvar) = &*pair_c;
        let mut cancelled = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + budget;
        loop {
            // Check first: stop() may have set this (and notified into the void)
            // before this thread even started, which happens constantly for fast
            // CDP commands where stop() is called right after spawn. Without this
            // top check the lost notify means we wait the full budget before
            // noticing, and stop()'s join() blocks for that whole time.
            if *cancelled {
                return;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                fired_c.store(true, std::sync::atomic::Ordering::SeqCst);
                handle.terminate_execution();
                return;
            }
            let (guard, _) = cvar.wait_timeout(cancelled, remaining).unwrap();
            cancelled = guard;
            if *cancelled {
                return;
            }
        }
    });
    WatchdogToken {
        pair,
        join: Some(join),
        fired,
    }
}

impl WatchdogToken {
    /// Stop the watchdog. Returns true if it had already fired (terminated the
    /// isolate). The caller must then clear the termination flag via
    /// [`ObscuraJsRuntime::cancel_termination`] before the next eval.
    pub fn stop(mut self) -> bool {
        {
            let (lock, cvar) = &*self.pair;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// Observation deadlines are checked between browser tasks. A task which has
// already started receives this bounded completion allowance, matching the
// fixed-wait path while retaining an absolute backstop for infinite script.
pub(crate) const SYNCHRONOUS_TASK_FLOOR_MS: u64 = 5_000;

impl Default for ObscuraJsRuntime {
    fn default() -> Self {
        Self::new()
    }
}
