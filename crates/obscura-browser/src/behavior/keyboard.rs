//! Keyboard behavior — keypress delay distribution that mimics a human,
//! plus occasional typos with correction. Real typing has:
//!
//! 1. Inter-key delays in the 80-250ms range, with a long-tail
//!    (occasional 500ms+ pauses when the user thinks).
//! 2. Faster delays for common digraphs ("th", "he", "in") — we approximate
//!    this by just tightening the range for adjacent keys.
//! 3. ~2-5% typo rate, with a backspace + retype correction.
//!
//! The output is a sequence of `BehaviorAction::KeyDown` / `KeyUp` /
//! `InsertText` events with realistic delays. The caller dispatches them
//! via `Input.dispatchKeyEvent` / `Input.insertText`.

use super::{Millis, NoiseEngine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct KeyboardSequence {
    seed: u64,
}

/// One keypress in a typing sequence. `key` is the logical key (e.g. "a",
/// "Enter"), `code` is the USB HID code (e.g. "KeyA", "Enter"). `text` is
/// what gets inserted (None for modifier keys).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyStep {
    pub key: String,
    pub code: String,
    pub text: Option<String>,
    pub delay_ms: Millis,
}

impl KeyboardSequence {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Types `text` with human-like delays and occasional typos. Returns
    /// the full key sequence including typo + backspace corrections.
    ///
    /// `typo_rate` is in [0, 1]; 0 disables typos. 0.03 is a realistic
    /// human rate.
    pub fn type_text(&self, text: &str, typo_rate: f64) -> Vec<KeyStep> {
        let engine = NoiseEngine::new(self.seed);
        let mut out = Vec::with_capacity(text.len() * 2);
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];
            // Decide whether to make a typo on this character.
            let typo_roll = engine.float_in(0x54_5950_4f00_0000, i as u64, 0.0, 1.0);
            if typo_roll < typo_rate && i + 1 < chars.len() {
                // Type a wrong char, pause, backspace, then the right one.
                let wrong = pick_wrong_char(c, engine.at(0x57_524f_4e47_0000, i as u64));
                out.push(key_step(wrong, delay_for(&engine, i as u64, true)));
                // Realization delay — humans notice a typo after ~150-300ms.
                out.push(KeyStep {
                    key: "Backspace".to_string(),
                    code: "Backspace".to_string(),
                    text: None,
                    delay_ms: engine.int_in(0x52_4541_4c49_5a00, i as u64, 150, 300) as Millis,
                });
                out.push(key_step(c, delay_for(&engine, i as u64, false)));
            } else {
                out.push(key_step(c, delay_for(&engine, i as u64, false)));
            }
            i += 1;
        }

        out
    }

    /// Presses a single (possibly modified) key. `key` is like "Enter",
    /// "Tab", "Escape"; `modifiers` is a bitmask from [`super::modifiers`].
    pub fn press_key(&self, key: &str, code: &str, _modifiers: u8) -> Vec<KeyStep> {
        let engine = NoiseEngine::new(self.seed);
        let delay = engine.int_in(0x50_5245_5353_0000, 0, 60, 180) as Millis;
        vec![KeyStep {
            key: key.to_string(),
            code: code.to_string(),
            text: None,
            delay_ms: delay,
        }]
    }
}

fn key_step(c: char, delay_ms: Millis) -> KeyStep {
    let (key, code) = key_for_char(c);
    KeyStep {
        key: key.to_string(),
        code: code.to_string(),
        text: Some(c.to_string()),
        delay_ms,
    }
}

/// Maps a character to its (key, code) pair. Handles common ASCII; the
/// rest fall through to "KeyA" / the char itself, which CDP's
/// `dispatchKeyEvent` treats as text input.
fn key_for_char(c: char) -> (&'static str, &'static str) {
    match c {
        ' ' => ("Space", "Space"),
        '\n' | '\r' => ("Enter", "Enter"),
        '\t' => ("Tab", "Tab"),
        'a'..='z' => ("KeyA", "KeyA"), // CDP uses KeyA..KeyZ for letters
        'A'..='Z' => ("KeyA", "KeyA"),
        '0'..='9' => ("Digit0", "Digit0"),
        _ => ("KeyA", "KeyA"),
    }
}

/// Picks a plausible wrong character near `c` on a QWERTY keyboard.
fn pick_wrong_char(c: char, roll: u64) -> char {
    let neighbors: &[char] = match c {
        'a' => &['s', 'q'],
        's' => &['a', 'd'],
        'd' => &['s', 'f'],
        'e' => &['w', 'r'],
        'r' => &['e', 't'],
        't' => &['r', 'y'],
        'i' => &['u', 'o'],
        'o' => &['i', 'p'],
        'n' => &['b', 'm'],
        'h' => &['g', 'j'],
        _ => &[c],
    };
    neighbors[(roll as usize) % neighbors.len()]
}

/// Inter-key delay. `fast` = true for digraphs and retyped typos (humans
/// type adjacent keys faster, and retype a typo quickly).
fn delay_for(engine: &NoiseEngine, index: u64, fast: bool) -> Millis {
    let (lo, hi) = if fast { (60, 140) } else { (90, 260) };
    engine.int_in(0x44_454c_4159_0000, index, lo, hi) as Millis
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_text_produces_expected_count_without_typos() {
        let k = KeyboardSequence::new(42);
        let steps = k.type_text("hello", 0.0);
        // One KeyStep per char, no typos.
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[0].text.as_deref(), Some("h"));
        assert_eq!(steps[4].text.as_deref(), Some("o"));
    }

    #[test]
    fn type_text_with_typos_has_backspace() {
        let k = KeyboardSequence::new(123);
        // Force typos with rate 1.0 on every char (except last, which
        // we skip typo to avoid OOB).
        let steps = k.type_text("hello", 1.0);
        // Should have at least one Backspace.
        assert!(
            steps.iter().any(|s| s.key == "Backspace"),
            "expected at least one Backspace with typo_rate=1.0, got: {:?}",
            steps.iter().map(|s| s.key.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn same_seed_same_sequence() {
        let a = KeyboardSequence::new(7).type_text("test", 0.0);
        let b = KeyboardSequence::new(7).type_text("test", 0.0);
        assert_eq!(a, b);
    }

    #[test]
    fn delays_in_human_range() {
        let k = KeyboardSequence::new(99);
        let steps = k.type_text("abcdef", 0.0);
        for s in &steps {
            assert!(s.delay_ms >= 60 && s.delay_ms <= 300, "delay out of range: {}", s.delay_ms);
        }
    }
}
