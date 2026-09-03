//! Proxy rotation — per-session and per-request proxy switching.
//!
//! Anti-bot vendors rate-limit by source IP. A single proxy is often not
//! enough — you want a pool that rotates. This module exposes:
//!
//! - [`ProxyPool`] — a list of proxy URLs with a rotation strategy
//! - [`RotationStrategy`] — `RoundRobin` (deterministic) or `Random`
//!   (seed-based, reproducible)
//! - [`ProxyPool::next_for`] — returns the proxy to use for a given key
//!   (e.g. a target host, or a session id). Same key → same proxy under
//!   `RoundRobin`; `Random` produces a stable pick per key too so a session
//!   keeps its proxy across navigations.
//!
//! The pool is a pure data structure. It does not perform network I/O and
//! does not know about `BrowserContext` — the caller wires a chosen proxy
//! into `BrowserContext.proxy_url` / `Page.http_client` when constructing
//! a new page or by calling [`crate::context::BrowserContext::set_proxy`].
//!
//! ## Sticky sessions
//!
//! `next_for("session-42")` always returns the same proxy for the same
//! pool, so a login flow that spans several requests keeps the same exit
//! IP. `next_for("host-accounts.x.ai")` rotates per-target so two targets
//! see different IPs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RotationStrategy {
    /// Pick proxies in order, wrapping around. Deterministic and easy to
    /// reason about. Use when you want even load across the pool.
    RoundRobin,
    /// Pick a proxy per key using a seed-based hash. Same key → same proxy,
    /// but the distribution across keys is uniform-ish. Use when you want
    /// stickiness per session/host without tracking state.
    Random,
}

/// A pool of proxy URLs with a rotation strategy.
///
/// The pool is `Clone` (cheap — `Arc<Mutex<...>>` inside) so multiple
/// workers can share one. The sticky-session map is also inside the mutex.
#[derive(Debug, Clone)]
pub struct ProxyPool {
    inner: std::sync::Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    proxies: Vec<String>,
    strategy: RotationStrategy,
    /// Sticky-session map: key (session id, host) → index into `proxies`.
    /// Kept inside the pool so two callers asking for the same key get the
    /// same proxy even under concurrent access.
    sticky: HashMap<String, usize>,
    /// Round-robin counter. Incremented on each `next_for` under
    /// `RoundRobin` so the next caller gets the next proxy.
    rr_counter: usize,
    /// Seed for the `Random` strategy. Mixed with the key to pick a stable
    /// index. Two pools with the same seed + proxies + strategy are
    /// interchangeable.
    seed: u64,
}

impl ProxyPool {
    pub fn new(proxies: Vec<String>, strategy: RotationStrategy, seed: u64) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(Inner {
                proxies,
                strategy,
                sticky: HashMap::new(),
                rr_counter: 0,
                seed,
            })),
        }
    }

    /// Returns the proxy URL to use for `key`. Returns `None` if the pool
    /// is empty.
    pub fn next_for(&self, key: &str) -> Option<String> {
        let mut inner = self.inner.lock().ok()?;
        if inner.proxies.is_empty() {
            return None;
        }
        // Sticky: if we've already picked for this key, return the same.
        if let Some(&idx) = inner.sticky.get(key) {
            return Some(inner.proxies[idx].clone());
        }
        let idx = match inner.strategy {
            RotationStrategy::RoundRobin => {
                let i = inner.rr_counter % inner.proxies.len();
                inner.rr_counter = inner.rr_counter.wrapping_add(1);
                i
            }
            RotationStrategy::Random => {
                // FNV-1a of (seed, key) → stable pick per key.
                let mut h: u64 = 0xcbf2_9ce3_6422_2325 ^ inner.seed;
                for b in key.bytes() {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(0x100_0000_01b3);
                }
                (h as usize) % inner.proxies.len()
            }
        };
        inner.sticky.insert(key.to_string(), idx);
        Some(inner.proxies[idx].clone())
    }

    /// Drops the sticky binding for `key`. The next `next_for(key)` will
    /// pick a fresh proxy. Use this to force-rotate a session that got
    /// rate-limited.
    pub fn invalidate(&self, key: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.sticky.remove(key);
        }
    }

    /// Clears all sticky bindings. The next `next_for` for any key picks
    /// a fresh proxy according to the strategy. Useful at the start of a
    /// new registration batch.
    pub fn reset(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.sticky.clear();
            inner.rr_counter = 0;
        }
    }

    /// Returns the number of proxies in the pool.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|i| i.proxies.len())
            .unwrap_or(0)
    }

    /// Returns true if the pool has no proxies.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a snapshot of all proxies (for tools/list or debugging).
    pub fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|i| i.proxies.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(n: usize, s: RotationStrategy) -> ProxyPool {
        let proxies: Vec<String> = (0..n).map(|i| format!("http://proxy{i}")).collect();
        ProxyPool::new(proxies, s, 42)
    }

    #[test]
    fn round_robin_rotates() {
        let p = pool(3, RotationStrategy::RoundRobin);
        let a = p.next_for("k1").unwrap();
        let b = p.next_for("k2").unwrap();
        let c = p.next_for("k3").unwrap();
        let d = p.next_for("k4").unwrap();
        // Four distinct keys under RoundRobin → first three cycle, fourth wraps.
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_eq!(a, d); // wrap-around
    }

    #[test]
    fn sticky_session_keeps_same_proxy() {
        let p = pool(3, RotationStrategy::RoundRobin);
        let a = p.next_for("session-1").unwrap();
        let b = p.next_for("session-1").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn invalidate_forces_rotation() {
        let p = pool(3, RotationStrategy::RoundRobin);
        let a = p.next_for("session-1").unwrap();
        p.invalidate("session-1");
        let b = p.next_for("session-1").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn random_strategy_is_stable_per_key() {
        let p = pool(10, RotationStrategy::Random);
        let a = p.next_for("session-1").unwrap();
        let b = p.next_for("session-1").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_pool_returns_none() {
        let p = ProxyPool::new(vec![], RotationStrategy::RoundRobin, 0);
        assert!(p.next_for("x").is_none());
        assert!(p.is_empty());
    }

    #[test]
    fn reset_clears_sticky() {
        let p = pool(3, RotationStrategy::RoundRobin);
        let _ = p.next_for("session-1");
        p.reset();
        // After reset, sticky map is empty but proxies are still there.
        assert_eq!(p.len(), 3);
    }
}
