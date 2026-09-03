//! High-level behavior planners that compose the mouse/keyboard/scroll
//! primitives into full action sequences. Each planner returns a
//! `Vec<BehaviorEvent>` ready for dispatch.
//!
//! These are the entry points the MCP layer and the Go client use; they
//! don't need to know about bezier curves or delay distributions.

use super::keyboard::KeyboardSequence;
use super::modifiers;
use super::mouse::{MouseButton, MouseSequence};
use super::scroll::ScrollSequence;
use super::{BehaviorAction, BehaviorEvent, Millis, NoiseEngine};

/// Plan a click at (x, y): move there along a curve, pause, press, release.
/// Returns the full event sequence.
pub fn plan_click(
    seed: u64,
    from: (f64, f64),
    to: (f64, f64),
    button: MouseButton,
) -> Vec<BehaviorEvent> {
    let mouse = MouseSequence::new(seed);
    let mut events = Vec::new();
    let (pre, hold) = mouse.click_timing((to.0 as u64, to.1 as u64));

    // Move phase.
    let mut first = true;
    for step in mouse.trail(from, to, 0) {
        if first {
            events.push(BehaviorEvent {
                delay_ms: step.delay_ms,
                action: BehaviorAction::MouseMove {
                    x: step.x,
                    y: step.y,
                },
            });
            first = false;
        } else {
            events.push(BehaviorEvent {
                delay_ms: step.delay_ms,
                action: BehaviorAction::MouseMove {
                    x: step.x,
                    y: step.y,
                },
            });
        }
    }

    // Pre-click pause.
    events.push(BehaviorEvent {
        delay_ms: pre,
        action: BehaviorAction::MouseDown {
            x: to.0,
            y: to.1,
            button,
        },
    });
    // Hold.
    events.push(BehaviorEvent {
        delay_ms: hold,
        action: BehaviorAction::MouseUp {
            x: to.0,
            y: to.1,
            button,
        },
    });
    events
}

/// Plan a pure mouse move (no click). Useful for hover.
pub fn plan_move(seed: u64, from: (f64, f64), to: (f64, f64)) -> Vec<BehaviorEvent> {
    let mouse = MouseSequence::new(seed);
    mouse
        .trail(from, to, 0)
        .into_iter()
        .map(|step| BehaviorEvent {
            delay_ms: step.delay_ms,
            action: BehaviorAction::MouseMove {
                x: step.x,
                y: step.y,
            },
        })
        .collect()
}

/// Plan typing `text` with realistic delays and optional typos.
pub fn plan_type(seed: u64, text: &str, typo_rate: f64) -> Vec<BehaviorEvent> {
    let kb = KeyboardSequence::new(seed);
    kb.type_text(text, typo_rate)
        .into_iter()
        .map(|step| BehaviorEvent {
            delay_ms: step.delay_ms,
            action: if let Some(text) = step.text {
                BehaviorAction::InsertText { text }
            } else {
                BehaviorAction::KeyDown {
                    key: step.key.clone(),
                    code: step.code.clone(),
                    modifiers: modifiers::NONE,
                }
            },
        })
        .collect()
}

/// Plan a scroll by `dy` pixels (positive = down).
pub fn plan_scroll(seed: u64, at: (f64, f64), dy: f64) -> Vec<BehaviorEvent> {
    let s = ScrollSequence::new(seed);
    s.scroll_by(dy)
        .into_iter()
        .map(|step| BehaviorEvent {
            delay_ms: step.delay_ms,
            action: BehaviorAction::Wheel {
                x: at.0,
                y: at.1,
                dx: step.dx,
                dy: step.dy,
            },
        })
        .collect()
}

/// Plan a key press (e.g. Enter, Tab, Escape) with optional modifiers.
pub fn plan_press_key(
    seed: u64,
    key: &str,
    code: &str,
    mods: u8,
) -> Vec<BehaviorEvent> {
    let kb = KeyboardSequence::new(seed);
    let steps = kb.press_key(key, code, mods);
    steps
        .into_iter()
        .flat_map(|step| {
            vec![
                BehaviorEvent {
                    delay_ms: step.delay_ms,
                    action: BehaviorAction::KeyDown {
                        key: step.key.clone(),
                        code: step.code.clone(),
                        modifiers: step_modifiers(step.key.as_str()),
                    },
                },
                BehaviorEvent {
                    delay_ms: 60,
                    action: BehaviorAction::KeyUp {
                        key: step.key.clone(),
                        code: step.code.clone(),
                        modifiers: step_modifiers(step.key.as_str()),
                    },
                },
            ]
        })
        .collect()
}

fn step_modifiers(key: &str) -> u8 {
    match key {
        "Shift" | "ShiftLeft" | "ShiftRight" => modifiers::SHIFT,
        "Control" | "ControlLeft" | "ControlRight" => modifiers::CTRL,
        "Alt" | "AltLeft" | "AltRight" => modifiers::ALT,
        "Meta" | "MetaLeft" | "MetaRight" => modifiers::META,
        _ => modifiers::NONE,
    }
}

/// Re-export the noise engine for callers that want to build their own
/// sequences from scratch.
pub fn noise_engine(seed: u64) -> NoiseEngine {
    NoiseEngine::new(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_sequence_has_move_down_and_up() {
        let events = plan_click(42, (0.0, 0.0), (100.0, 100.0), MouseButton::Left);
        let has_move = events.iter().any(|e| matches!(
            e.action,
            BehaviorAction::MouseMove { .. }
        ));
        let has_down = events.iter().any(|e| matches!(
            e.action,
            BehaviorAction::MouseDown { .. }
        ));
        let has_up = events.iter().any(|e| matches!(
            e.action,
            BehaviorAction::MouseUp { .. }
        ));
        assert!(has_move && has_down && has_up);
    }

    #[test]
    fn type_sequence_emits_insert_text() {
        let events = plan_type(42, "hi", 0.0);
        let inserts: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.action {
                BehaviorAction::InsertText { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(inserts, vec!["h".to_string(), "i".to_string()]);
    }

    #[test]
    fn scroll_sequence_has_wheel_events() {
        let events = plan_scroll(42, (100.0, 100.0), 500.0);
        assert!(events.iter().all(|e| matches!(
            e.action,
            BehaviorAction::Wheel { .. }
        )));
        assert!(!events.is_empty());
    }

    #[test]
    fn same_seed_same_plan() {
        let a = plan_click(7, (0.0, 0.0), (100.0, 100.0), MouseButton::Left);
        let b = plan_click(7, (0.0, 0.0), (100.0, 100.0), MouseButton::Left);
        assert_eq!(a, b);
    }
}
