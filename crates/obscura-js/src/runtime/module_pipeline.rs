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


    pub async fn load_module(&mut self, url: &str, budget_ms: u64) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(budget_ms);
        let prepared = self.prepare_module(url, budget_ms).await?;
        let remaining_ms = remaining_deadline_ms(deadline).ok_or_else(|| {
            format!(
                "Module {} exhausted its {}ms load+evaluation budget",
                url, budget_ms
            )
        })?;
        self.evaluate_prepared_module(prepared, remaining_ms).await
    }

    pub async fn prepare_module(
        &mut self,
        url: &str,
        budget_ms: u64,
    ) -> Result<PreparedModule, String> {
        let budget = tokio::time::Duration::from_millis(budget_ms);
        let specifier = deno_core::ModuleSpecifier::parse(url)
            .map_err(|e| format!("Invalid module URL {}: {}", url, e))?;
        let loaded_start = self.loaded_module_specifiers.borrow().len();

        // Bound the recursive import-graph fetch. deno_core fetches the graph
        // concurrently through the one page-scoped module loader. Loading the
        // entry from that loader too is important: cookies, configured request
        // headers, redirects, interception, and callbacks must not change at
        // the first import edge.
        // The caller sizes the budget: short for enhancement modules on an
        // already-rendered page, full for an unmounted SPA shell (#205).
        let module_id = match tokio::time::timeout(
            budget,
            self.runtime.load_side_es_module(&specifier),
        )
        .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => return Err(format!("Module load error: {}", e)),
            Err(_) => {
                return Err(format!(
                    "Module graph load timed out after {}ms: {}",
                    budget_ms, url
                ));
            }
        };

        // Return as soon as the module finishes evaluating rather than waiting
        // for the loop to go fully idle: a page timer (setInterval) keeps the
        // loop busy forever and would otherwise burn the whole budget (#374).
        let mut graph_specifiers = self.loaded_module_specifiers.borrow()[loaded_start..].to_vec();
        graph_specifiers.push(specifier.to_string());
        graph_specifiers.sort_unstable();
        graph_specifiers.dedup();

        Ok(PreparedModule {
            module_id,
            description: format!("Module {}", url),
            entry_specifier: Some(specifier.to_string()),
            graph_specifiers,
        })
    }

    /// Drive a just-started module evaluation to completion, or up to
    /// `budget_ms`. Returns as soon as the module finishes rather than waiting
    /// for the event loop to go idle: a page timer (setInterval) keeps the loop
    /// busy forever and would otherwise burn the whole budget, abandoning a
    /// module that had already evaluated (issue #374).
    ///
    /// A module eval error or timeout is returned to the page lifecycle. The
    /// caller may continue rendering, but must not report a failed module as
    /// successfully loaded. An event-loop error is propagated out of the
    /// select and handled the same way.
    pub(crate) async fn drive_module_eval(
        &mut self,
        module_id: deno_core::ModuleId,
        budget_ms: u64,
        what: &str,
    ) -> Result<(), String> {
        if let Some(outcome) = self.module_evaluations.get(&module_id) {
            return outcome.clone();
        }

        self.begin_javascript_task();
        let budget = tokio::time::Duration::from_millis(budget_ms);
        // deno_core 0.350 asserts instead of treating a second evaluation as
        // the module-map no-op required by browsers. The local outcome cache
        // covers duplicate roots prepared by Obscura. A root can also have
        // been evaluated earlier as another graph's dependency, which is only
        // observable when mod_evaluate checks V8's private module status, so
        // contain that dependency assertion at this boundary as well.
        let evaluation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.runtime.mod_evaluate(module_id)
        }));
        let result = match evaluation {
            Ok(result) => result,
            Err(payload) => {
                let message = panic_payload_message(payload.as_ref());
                let outcome = if message.contains("Module already evaluated") {
                    Ok(())
                } else {
                    Err(format!("{} evaluation panicked: {}", what, message))
                };
                self.module_evaluations.insert(module_id, outcome.clone());
                return outcome;
            }
        };
        tokio::pin!(result);

        let outcome = tokio::time::timeout(budget, async {
            let event_loop = self
                .runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default());
            tokio::pin!(event_loop);
            tokio::select! {
                biased;
                e = &mut event_loop => { e?; (&mut result).await }
                r = &mut result => r,
            }
        })
        .await;

        let outcome = match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{} eval error: {}", what, e)),
            Err(_) => Err(format!(
                "{} evaluation timed out after {}ms",
                what, budget_ms
            )),
        };
        let outcome = self.finish_heap_checked(outcome);
        self.module_evaluations.insert(module_id, outcome.clone());
        outcome
    }

    pub async fn load_inline_module(
        &mut self,
        code: &str,
        base_url: &str,
        budget_ms: u64,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(budget_ms);
        let prepared = self
            .prepare_inline_module(code, base_url, budget_ms)
            .await?;
        let remaining_ms = remaining_deadline_ms(deadline).ok_or_else(|| {
            format!(
                "Inline module exhausted its {}ms load+evaluation budget",
                budget_ms
            )
        })?;
        self.evaluate_prepared_module(prepared, remaining_ms).await
    }

    pub async fn prepare_inline_module(
        &mut self,
        code: &str,
        base_url: &str,
        budget_ms: u64,
    ) -> Result<PreparedModule, String> {
        let budget = tokio::time::Duration::from_millis(budget_ms);
        // Inline modules use the document base URL as their module URL. This is
        // observable through import.meta.url and is also the referrer used for
        // relative imports and import-map scope matching. deno_core permits
        // multiple side modules with this name; the returned ModuleId keeps
        // each prepared module distinct until its scheduled evaluation.
        let specifier = deno_core::ModuleSpecifier::parse(base_url)
            .unwrap_or_else(|_| deno_core::ModuleSpecifier::parse("about:blank").unwrap());
        let loaded_start = self.loaded_module_specifiers.borrow().len();

        let module_id = match tokio::time::timeout(
            budget,
            self.runtime.load_side_es_module_from_code(
                &specifier,
                deno_core::ModuleCodeString::from(code.to_string()),
            ),
        )
        .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => return Err(format!("Inline module load error: {}", e)),
            Err(_) => {
                return Err(format!(
                    "Inline module graph load timed out after {}ms",
                    budget_ms
                ));
            }
        };

        // Return as soon as the module finishes evaluating rather than waiting
        // for idle: Vite's HMR / React-Refresh client installs a setInterval that
        // keeps the loop busy forever, and waiting for idle burned the whole
        // budget on this preamble module and starved the module that mounts the
        // app, leaving #root empty (issue #374).
        let mut graph_specifiers = self.loaded_module_specifiers.borrow()[loaded_start..].to_vec();
        graph_specifiers.sort_unstable();
        graph_specifiers.dedup();

        Ok(PreparedModule {
            module_id,
            description: "Inline module".to_string(),
            // Multiple inline modules intentionally share the document URL,
            // but each has its own source and ModuleId.
            entry_specifier: None,
            graph_specifiers,
        })
    }

    pub async fn evaluate_prepared_module(
        &mut self,
        prepared: PreparedModule,
        budget_ms: u64,
    ) -> Result<(), String> {
        let PreparedModule {
            module_id,
            description,
            entry_specifier,
            graph_specifiers,
        } = prepared;
        if let Some(outcome) = entry_specifier
            .as_ref()
            .and_then(|specifier| self.evaluated_module_specifiers.get(specifier))
        {
            return outcome.clone();
        }
        // Tokio timeouts cannot run while synchronous top-level module work
        // pins the runtime thread in V8. Pair the async timeout with a hard V8
        // watchdog so this budget is a real wall-clock ceiling for both forms
        // of evaluation.
        let watchdog = self.arm_watchdog(std::time::Duration::from_millis(budget_ms));
        let result = self
            .drive_module_eval(module_id, budget_ms, &description)
            .await;
        let watchdog_fired = self.disarm_watchdog(watchdog);
        let result = if watchdog_fired {
            Err(format!(
                "{} evaluation timed out after {}ms",
                description, budget_ms
            ))
        } else {
            result
        };

        if let Some(entry_specifier) = entry_specifier {
            self.evaluated_module_specifiers
                .insert(entry_specifier, result.clone());
        }
        if result.is_ok() {
            for specifier in graph_specifiers {
                self.evaluated_module_specifiers.insert(specifier, Ok(()));
            }
        }
        result
    }

}
