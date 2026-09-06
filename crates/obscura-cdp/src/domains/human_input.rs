//! Humanized keyboard input pipeline.
//!
//! Mirrors the gesture channel: one `Input.humanType` CDP command carries the
//! full text plus per-character delays, and the page replays the complete
//! per-key event sequence (keydown → beforeinput → keypress → input → keyup)
//! on a setTimeout chain. Interactive typing must not go through
//! Runtime.evaluate — its post-eval frame pump stalls seconds behind busy
//! challenge pages, exactly like it did for trajectories.

use serde_json::json;

pub fn handle(
    method: &str,
    params: &serde_json::Value,
    ctx: &mut crate::dispatch::CdpContext,
    session_id: &Option<String>,
) -> Result<serde_json::Value, String> {
    match method {
        "humanType" => {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("text required")?
                .to_string();
            let delays: Vec<u64> = params
                .get("delays")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_u64())
                        .collect()
                })
                .unwrap_or_default();
            // Optional JS expression selecting the target element; defaults to
            // the active element (real typing goes wherever focus is).
            let focus_expr = params
                .get("focusExpr")
                .and_then(|v| v.as_str())
                .unwrap_or("document.activeElement")
                .to_string();
            let clear_first = params
                .get("clear")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let seq_base = ctx.next_input_seq();

            if let Some(page) = ctx.get_session_page_mut(session_id) {
                let mut js = String::with_capacity(text.len() * 160 + 2048);
                js.push_str("(function(){\n");
                js.push_str(&format!(
                    "  var target = ({focus_expr});\n  if (!target) return;\n  target.focus && target.focus();\n"
                ));
                js.push_str(&format!(
                    "  var seq = {seq_base};\n  var text = {text};\n",
                    seq_base = seq_base,
                    // serde-escaped JSON string literal
                    text = json!(text),
                ));
                if clear_first {
                    js.push_str(
                        "  if ('value' in target) { globalThis.__obscura_setFieldValue(target, 'value', ''); }\n",
                    );
                }
                js.push_str(
                    "  var delays = [__DELAYS__];\n\
                     \x20 var i = 0;\n\
                     \x20 function fire(type, opts, after) {\n\
                     \x20   seq++;\n\
                     \x20   try { globalThis.__obscura_engineInput({kind:type, seq:seq, ts:performance.now(), prev:[]}); } catch(e) {}\n\
                     \x20   var ev = globalThis.__obscura_markTrusted(new KeyboardEvent(type, opts));\n\
                     \x20   try { Object.defineProperty(ev, '_engineSeq', {value:seq}); } catch(e) {}\n\
                     \x20   target.dispatchEvent(ev);\n\
                     \x20   if (after) after();\n\
                     \x20 }\n\
                     \x20 function typeChar() {\n\
                     \x20   if (i >= text.length) { document.title = (document.__tyDone = true, document.title); return; }\n\
                     \x20   var ch = text[i];\n\
                     \x20   var code = ch >= 'a' && ch <= 'z' ? 'Key' + ch.toUpperCase() :\n\
                     \x20              ch >= 'A' && ch <= 'Z' ? 'Key' + ch :\n\
                     \x20              ch >= '0' && ch <= '9' ? 'Digit' + ch : '';\n\
                     \x20   var needsShift = ch >= 'A' && ch <= 'Z' || '~!@#$%^&*()_+{}|:\"<>?'.indexOf(ch) >= 0;\n\
                     \x20   var d = delays[i] || 90;\n\
                     \x20   fire('keydown', {key:ch, code:code, bubbles:true, cancelable:true, shiftKey:needsShift});\n\
                     \x20   try {\n\
                     \x20     var bi = globalThis.__obscura_markTrusted(new Event('beforeinput', {bubbles:true, cancelable:true}));\n\
                     \x20     target.dispatchEvent(bi);\n\
                     \x20   } catch(e) {}\n\
                     \x20   if (ch === '\\n' && target.localName === 'textarea') {\n\
                     \x20     globalThis.__obscura_setFieldValue(target, 'value', (target.value || '') + '\\n');\n\
                     \x20   } else if ('value' in target) {\n\
                     \x20     globalThis.__obscura_setFieldValue(target, 'value', (target.value || '') + ch);\n\
                     \x20   }\n\
                     \x20   if (code) fire('keypress', {key:ch, code:code, bubbles:true, cancelable:true, charCode:ch.charCodeAt(0), keyCode:ch.charCodeAt(0), which:ch.charCodeAt(0), shiftKey:needsShift});\n\
                     \x20   try { target.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {bubbles:true}))); } catch(e) {}\n\
                     \x20   fire('keyup', {key:ch, code:code, bubbles:true, cancelable:true, shiftKey:needsShift});\n\
                     \x20   i++;\n\
                     \x20   setTimeout(typeChar, d);\n\
                     \x20 }\n\
                     \x20 typeChar();\n\
                     })()",
                );
                // delays array literal
                let delays_lit = if delays.is_empty() {
                    "90".to_string()
                } else {
                    delays
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                js = js.replace("__DELAYS__", &delays_lit);
                page.evaluate(&js);
            }
            Ok(json!({}))
        }
        _ => Err(format!("Unknown Input method: {}", method)),
    }
}
