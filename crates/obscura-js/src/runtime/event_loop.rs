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


    /// Drive the event loop for at most `budget_ms`, bounded against BOTH async
    /// idle (Tokio deadline) and synchronous hangs (V8 watchdog). The deadline
    /// is observed between browser tasks; a task already running there gets a
    /// five-second completion allowance plus a 500ms scheduling margin before
    /// the watchdog terminates it. A well-behaved page returns as soon as the
    /// loop goes idle.
    pub async fn run_event_loop_bounded(&mut self, budget_ms: u64) -> Result<(), String> {
        if budget_ms == 0 {
            return self.run_event_loop().await;
        }
        let budget = std::time::Duration::from_millis(budget_ms);
        let deadline = tokio::time::Instant::now() + budget;
        // A capture/readiness deadline is observed only between browser tasks.
        // Chromium does not terminate the JavaScript task which happens to be
        // active when a screenshot delay expires; the capture waits for that
        // task boundary. Keep a separate long-task floor so short compositor
        // slices and explicit waits do not kill legitimate framework work,
        // while an actually unyielding task remains bounded.
        // One watchdog for the complete pump avoids spawning a native thread
        // per cooperative task. Adding the floor after the observation budget
        // guarantees that even a task beginning just before `deadline` gets
        // the same bounded completion allowance.
        let synchronous_budget = budget
            .saturating_add(std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS));
        let token =
            self.arm_watchdog(synchronous_budget
                + std::time::Duration::from_millis(WATCHDOG_SCHEDULING_MARGIN_MS));
        let result = loop {
            if tokio::time::Instant::now() >= deadline {
                break Ok(());
            }

            match tokio::time::timeout_at(deadline, self.run_cooperative_event_loop_tick()).await {
                Ok(Ok(true)) => break Ok(()),
                Ok(Ok(false)) => {
                    // End-of-task microtasks belong to this turn, but work
                    // queued from them belongs to a subsequent cooperative
                    // turn. Yield so the wall deadline remains observable even
                    // when every turn immediately schedules another one.
                    self.runtime.v8_isolate().perform_microtask_checkpoint();
                    tokio::task::yield_now().await;
                }
                Ok(Err(error)) => break Err(error),
                Err(_) => break Ok(()),
            }
        };
        let fired = self.disarm_watchdog(token);
        match result {
            Err(error) if error.contains("heap limit exceeded") => Err(error),
            Err(error) if fired || error.contains("execution terminated") => Ok(()),
            other => other,
        }
    }

    /// Drive page tasks for a fixed observation interval without asking
    /// deno_core's run-to-idle future to own that entire interval.
    ///
    /// Modern schedulers commonly keep the event loop continuously ready with
    /// animation frames, zero-delay tasks, or streaming work. A single
    /// `run_event_loop()` poll then never yields to Tokio, so the fixed-delay
    /// deadline can only be enforced by terminating otherwise valid page JS.
    /// Cooperative turns preserve the requested wall interval while returning
    /// to the embedder between task-queue wakes. The watchdog remains solely as
    /// a backstop for one genuinely synchronous, unyielding turn.
    pub async fn run_event_loop_for_duration(&mut self, budget_ms: u64) -> Result<(), String> {
        if budget_ms == 0 {
            return Ok(());
        }
        self.run_event_loop_bounded(budget_ms).await
    }

    /// Drive one deno_core event-loop tick at a time. When the first tick
    /// parks, process one more tick after its registered waker fires, then
    /// yield back to the embedder even if that tick schedules more work.
    ///
    /// `JsRuntime::run_event_loop()` is a run-to-idle future. When a page keeps
    /// it continuously ready (zero-delay schedulers, streaming traffic, or a
    /// framework work queue), Tokio never regains control to observe a timeout
    /// or our readiness policy. This future deliberately turns the wake for a
    /// second tick into a return to the caller. If no work is immediately
    /// ready, it remains parked on deno_core's real I/O/timer waker, so the
    /// adaptive settle loop does not poll at a fixed frequency.
    pub(crate) async fn run_cooperative_event_loop_tick(&mut self) -> Result<bool, String> {
        self.begin_javascript_task();
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        let mut waiting_for_wake = false;
        let result = std::future::poll_fn(|cx| {
            let tick = self
                .runtime
                .poll_event_loop(cx, deno_core::PollEventLoopOptions::default());
            match tick {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(true)),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(format!(
                    "Event loop error: {error}"
                ))),
                std::task::Poll::Pending if waiting_for_wake => {
                    std::task::Poll::Ready(Ok(false))
                }
                std::task::Poll::Pending => {
                    waiting_for_wake = true;
                    std::task::Poll::Pending
                }
            }
        })
        .await;
        self.finish_heap_checked(result)
    }

    /// Drive one browser task while allowing the future to remain parked on
    /// deno_core's real timer/network waker. This is the long-lived browser
    /// server counterpart to bounded screenshot settling: the owner selects
    /// this future alongside incoming protocol commands, so a page continues
    /// to make progress while the automation client is idle without polling at
    /// a fixed frequency.
    ///
    /// The shared CDP watchdog is armed only around synchronous V8 entry. It is
    /// deliberately disarmed while `Poll::Pending`; a legitimate distant timer
    /// must not look like a hung JavaScript task merely because the runtime is
    /// asleep waiting for it.
    #[doc(hidden)]
    pub async fn run_autonomous_event_loop_turn(&mut self) -> Result<bool, String> {
        const AUTONOMOUS_TASK_WATCHDOG_MS: u64 =
            SYNCHRONOUS_TASK_FLOOR_MS + WATCHDOG_SCHEDULING_MARGIN_MS;

        self.begin_javascript_task();

        let checkpoint_watchdog = crate::cdp_watchdog::arm(
            self.isolate_handle(),
            std::time::Duration::from_millis(AUTONOMOUS_TASK_WATCHDOG_MS),
        );
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        if crate::cdp_watchdog::disarm(checkpoint_watchdog) {
            self.cancel_termination();
            return Err("autonomous microtask checkpoint exceeded its task budget".into());
        }
        if self.recover_heap_limit() {
            return Err("JavaScript heap limit exceeded; execution terminated".into());
        }

        let isolate_handle = self.isolate_handle();
        let mut waiting_for_wake = false;
        let result = std::future::poll_fn(|cx| {
            let watchdog = crate::cdp_watchdog::arm(
                isolate_handle.clone(),
                std::time::Duration::from_millis(AUTONOMOUS_TASK_WATCHDOG_MS),
            );
            let tick = self
                .runtime
                .poll_event_loop(cx, deno_core::PollEventLoopOptions::default());
            let watchdog_fired = crate::cdp_watchdog::disarm(watchdog);
            if watchdog_fired {
                self.runtime.v8_isolate().cancel_terminate_execution();
                return std::task::Poll::Ready(Err(
                    "autonomous browser task exceeded its task budget".into(),
                ));
            }
            match tick {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(true)),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(format!(
                    "Event loop error: {error}"
                ))),
                std::task::Poll::Pending if waiting_for_wake => {
                    std::task::Poll::Ready(Ok(false))
                }
                std::task::Poll::Pending => {
                    waiting_for_wake = true;
                    std::task::Poll::Pending
                }
            }
        })
        .await;
        self.finish_heap_checked(result)
    }

    /// Drive one cooperative event-loop turn for browser lifecycle code that
    /// must re-check an external readiness predicate after every wake. The
    /// boolean is true only when deno_core reached full idle.
    #[doc(hidden)]
    pub async fn run_load_delaying_event_loop_tick(&mut self) -> Result<bool, String> {
        self.run_cooperative_event_loop_tick().await
    }

    /// Pump deferred work until deno_core reports true idle, or until the page
    /// has had no connected-document mutation, relevant request/dynamic-script
    /// work, or near-term one-shot timeout for `quiet_ms`. Network and script
    /// work gets a bounded post-load grace period: this retains ordinary app
    /// hydration without allowing analytics, telemetry, or a hung endpoint to
    /// consume the caller's complete budget. Long timers and perpetual visual
    /// mutations are bounded separately for the same reason.
    /// `budget_ms` remains an absolute wall-clock bound.
    pub async fn run_event_loop_until_quiescent(
        &mut self,
        budget_ms: u64,
        quiet_ms: u64,
    ) -> Result<(), String> {
        if budget_ms == 0 {
            return Ok(());
        }

        let budget = std::time::Duration::from_millis(budget_ms);
        let quiet = std::time::Duration::from_millis(quiet_ms.max(1).min(budget_ms));
        let started = tokio::time::Instant::now();
        let deadline = started + budget;
        // A one-second grace covers the common load -> fetch -> framework
        // commit path (and matches the CLI's established one-second useful
        // hydration window), but it is intentionally independent of a larger
        // caller budget. Requests which remain pending after this point are no
        // longer readiness evidence by themselves. Their eventual connected
        // DOM mutation is still observed during the bounded activity tail.
        const EXTERNAL_WORK_GRACE_MS: u64 = 1_000;
        const OBSERVABLE_ACTIVITY_TAIL_MS: u64 = 500;
        let external_work_grace =
            std::time::Duration::from_millis(EXTERNAL_WORK_GRACE_MS).min(budget);
        let external_work_deadline = started + external_work_grace;
        let activity_tail = std::time::Duration::from_millis(OBSERVABLE_ACTIVITY_TAIL_MS);
        let mut activity_deadline = deadline.min(started + activity_tail);
        let token = self.arm_watchdog(
            budget
                .saturating_add(std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS))
                + std::time::Duration::from_millis(WATCHDOG_SCHEDULING_MARGIN_MS),
        );
        let mut generation = self.activity_generation();
        let mut quiet_since: Option<tokio::time::Instant> = None;
        let result = loop {
            let now = tokio::time::Instant::now();
            let Some(_remaining) = deadline.checked_duration_since(now) else {
                break Ok(());
            };
            let next_generation = self.activity_generation();
            // One-shot timers up to two quiet windows away are commonly app
            // hydration/debounce work (`setTimeout(render, 200)`). Intervals
            // are intentionally excluded, and distant one-shots are treated
            // like Chromium after `load`: callers needing an arbitrary fixed
            // delay can request strict settle.
            let near_timeout = self
                .next_pending_timeout_delay_ms()
                .is_some_and(|delay| delay <= quiet.as_secs_f64() * 2_000.0);
            let external_work_pending = now < external_work_deadline
                && (self.has_pending_network_requests() || self.has_pending_dynamic_scripts());
            if external_work_pending {
                activity_deadline = deadline.min(external_work_deadline + activity_tail);
                generation = next_generation;
                quiet_since = None;
            } else if now < activity_deadline && near_timeout {
                generation = next_generation;
                quiet_since = None;
            } else {
                if now < activity_deadline && next_generation != generation {
                    // A mutation starts a fresh quiet interval at its observed
                    // delivery time. There is no need for a fixed-rate poll to
                    // discover that the interval has begun.
                    quiet_since = Some(now);
                }
                generation = next_generation;
                let since = quiet_since.get_or_insert(now);
                if now.duration_since(*since) >= quiet {
                    break Ok(());
                }
            }

            // Park on the runtime's actual waker. The policy deadline is only
            // a fallback for a hung request, a quiet-window expiry, or the
            // caller's absolute budget; it is not a periodic polling quantum.
            let policy_deadline = if external_work_pending {
                external_work_deadline
            } else if now < activity_deadline && near_timeout {
                activity_deadline
            } else {
                quiet_since.map_or(deadline, |since| since + quiet)
            }
            .min(deadline);
            // deno_core's public poll is one event-loop iteration, but an
            // iteration may synchronously drain an arbitrarily long chain of
            // nextTick/macrotask/microtask callbacks before returning. Tokio's
            // deadline cannot preempt that native V8 call. Bound the individual
            // turn beyond the readiness horizon by the same bounded task
            // allowance as fixed waits. The observation window may expire
            // while valid framework/layout work is running; browser capture
            // waits for that task boundary instead of terminating it midway.
            let tick_watchdog = self.arm_watchdog(
                policy_deadline.saturating_duration_since(now)
                    + std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS)
                    + std::time::Duration::from_millis(WATCHDOG_SCHEDULING_MARGIN_MS),
            );
            let tick = tokio::time::timeout_at(
                policy_deadline,
                self.run_cooperative_event_loop_tick(),
            )
            .await;
            let tick_fired = self.disarm_watchdog(tick_watchdog);
            if tick_fired {
                break Ok(());
            }
            self.runtime.v8_isolate().perform_microtask_checkpoint();
            match tick {
                Ok(Ok(true)) => break Ok(()),
                Ok(Ok(false)) | Err(_) => {}
                Ok(Err(error)) => break Err(error),
            }
        };
        let fired = self.disarm_watchdog(token);
        match result {
            Err(error) if error.contains("heap limit exceeded") => Err(error),
            Err(error) if fired || error.contains("execution terminated") => Ok(()),
            other => other,
        }
    }

    /// Like [`Self::evaluate`] but bounded by a V8 watchdog, so a `--eval`
    /// expression that loops forever (or awaits a promise that never settles in
    /// synchronous form) cannot hang the process.
    pub fn evaluate_with_timeout(
        &mut self,
        expression: &str,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        if timeout.is_zero() {
            return self.evaluate(expression);
        }
        self.begin_javascript_task();
        let wrapped = Self::wrap_expression(expression);
        let token = self.arm_watchdog(timeout);
        let result = self.runtime.execute_script("<eval>", wrapped);
        let fired = self.disarm_watchdog(token);
        if self.recover_heap_limit() {
            return Err("JavaScript heap limit exceeded; execution terminated".to_string());
        }
        match result {
            Ok(v) if !fired => self.v8_to_json(v),
            Ok(_) => Err("eval timed out".to_string()),
            Err(e) => {
                let msg = e.to_string();
                if fired || msg.contains("execution terminated") {
                    Err("eval timed out".to_string())
                } else {
                    Err(format!("JS error: {}", msg))
                }
            }
        }
    }

    pub async fn resolve_promises(&mut self) {
        self.begin_javascript_task();
        // Default settle: just pump until idle or 5s.
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            self.runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default()),
        )
        .await;
        self.recover_heap_limit();
    }

    /// Pump the event loop until `done_check` returns true (e.g. an IIFE
    /// has written its result sentinel), or `max_total_ms` elapses. Returns
    /// whether the predicate completed before the deadline.
    ///
    /// Why this exists: `run_event_loop(default)` only returns when there is
    /// no pending work. Page JS routinely schedules long setTimeouts
    /// (IntersectionObserver re-fires at 7s, requestIdleCallback, etc.) that
    /// the caller does not care about. With the plain timeout we waited 5s
    /// even when the IIFE we cared about resolved in <1ms — the click flow
    /// added ~7s per click because Puppeteer's `isIntersectingViewport`
    /// disconnects its observer in the callback, but our scheduled
    /// re-fires keep the event loop "busy" until they all fire.
    pub async fn resolve_promises_until<F>(
        &mut self,
        mut done_check: F,
        max_total_ms: u64,
    ) -> bool
    where
        F: FnMut(&mut Self) -> bool,
    {
        let deadline =
            tokio::time::Instant::now() + tokio::time::Duration::from_millis(max_total_ms);
        let mut tick_ms: u64 = 1;
        loop {
            self.begin_javascript_task();
            if done_check(self) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            // Pump for a short slice. If the loop returns idle in <tick_ms,
            // run_event_loop returns Ok and we check the predicate again.
            let _ = tokio::time::timeout(
                tokio::time::Duration::from_millis(tick_ms),
                self.runtime
                    .run_event_loop(deno_core::PollEventLoopOptions::default()),
            )
            .await;
            if self.recover_heap_limit() {
                return false;
            }
            // Backoff so a hung promise doesn't burn CPU. Caps at 50ms;
            // worst case we miss the result by <50ms.
            if tick_ms < 50 {
                tick_ms = (tick_ms * 2).min(50);
            }
        }
    }

// IMPL-WRAP-END
}
