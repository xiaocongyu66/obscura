use serde_json::{json, Value};

use crate::dispatch::CdpContext;

// Insert `text` at the caret, replacing any non-collapsed selection the way a
// real browser does when you type over selected text (for example after a
// triple-click select-all). selectionStart is null during ordinary typing, so
// the legacy append path is kept when no selection is tracked.
//
// The text is embedded as a JSON string literal rather than escaped by hand
// into single quotes. JSON string syntax is a subset of JavaScript's, so this
// covers the quote and the backslash of issue #433 and the control characters
// they left out: a newline inside a single-quoted literal is a syntax error,
// so the whole snippet was dropped and nothing was inserted. obscura-mcp
// already builds its typing snippet this way.
fn insert_text_js(text: &str) -> String {
    let literal = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(function() {{\
            var t = document.activeElement;\
            if (!t || (t.localName !== 'input' && t.localName !== 'textarea')) return;\
            var ins = {text};\
            var v = t.value || '';\
            var s = t.selectionStart, e = t.selectionEnd;\
            if (s == null) {{\
                globalThis.__obscura_setFieldValue(t, 'value', v + ins);\
            }} else {{\
                s = Math.max(0, Math.min(s, v.length));\
                e = (e == null) ? s : Math.max(0, Math.min(e, v.length));\
                var lo = Math.min(s, e), hi = Math.max(s, e);\
                globalThis.__obscura_setFieldValue(t, 'value', v.slice(0, lo) + ins + v.slice(hi));\
                var caret = lo + ins.length;\
                t.setSelectionRange(caret, caret);\
            }}\
            t.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {{bubbles:true}})));\
        }})()",
        text = literal,
    )
}

// Backspace deletes the selected range when there is one, so the common
// "triple-click to select-all, then Backspace to clear" pattern works. With a
// collapsed caret it removes the character before the caret, and with no
// selection tracked it falls back to trimming the last character (legacy).
const BACKSPACE_JS: &str = "(function() {\
    var t = document.activeElement;\
    if (!t || (t.localName !== 'input' && t.localName !== 'textarea')) return;\
    var v = t.value || '';\
    var s = t.selectionStart, e = t.selectionEnd;\
    if (s == null) {\
        globalThis.__obscura_setFieldValue(t, 'value', v.slice(0, -1));\
    } else {\
        s = Math.max(0, Math.min(s, v.length));\
        e = (e == null) ? s : Math.max(0, Math.min(e, v.length));\
        if (s !== e) {\
            var lo = Math.min(s, e), hi = Math.max(s, e);\
            globalThis.__obscura_setFieldValue(t, 'value', v.slice(0, lo) + v.slice(hi));\
            t.setSelectionRange(lo, lo);\
        } else if (s > 0) {\
            globalThis.__obscura_setFieldValue(t, 'value', v.slice(0, s - 1) + v.slice(s));\
            t.setSelectionRange(s - 1, s - 1);\
        }\
    }\
    t.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {bubbles:true})));\
})()";

fn mouse_button_code(button: &str) -> u8 {
    match button {
        "middle" => 1,
        "right" => 2,
        "back" => 3,
        "forward" => 4,
        _ => 0,
    }
}

fn mouse_button_mask(button: &str) -> u64 {
    match button {
        "right" => 2,
        "middle" => 4,
        "back" => 8,
        "forward" => 16,
        "none" => 0,
        _ => 1,
    }
}

fn modifier_flags(modifiers: u64) -> (bool, bool, bool, bool) {
    // CDP Input.Modifier: Alt=1, Ctrl=2, Meta=4, Shift=8.
    (
        modifiers & 1 != 0,
        modifiers & 2 != 0,
        modifiers & 4 != 0,
        modifiers & 8 != 0,
    )
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "dispatchMouseEvent" => {
            let event_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let button = params.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let button_code = mouse_button_code(button);
            let buttons = params
                .get("buttons")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| mouse_button_mask(button));
            let click_count = params.get("clickCount").and_then(|v| v.as_u64()).unwrap_or(1);
            let modifiers = params.get("modifiers").and_then(|v| v.as_u64()).unwrap_or(0);
            let (alt_key, ctrl_key, meta_key, shift_key) = modifier_flags(modifiers);

            if event_type == "mousePressed" {
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || globalThis.__obscura_click_target || document.activeElement || document.body;\
                            if (!target) return;\
                            globalThis.__obscura_click_target = target;\
                            globalThis.__obscura_mouse_down = {{target:target,button:{button_code},clickCount:{click_count}}};\
                            var evt = globalThis.__obscura_markTrusted(new MouseEvent('mousedown', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:{buttons},detail:{click_count},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            target.dispatchEvent(evt);\
                        }})()",
                        x = x,
                        y = y,
                        button_code = button_code,
                        buttons = buttons,
                        click_count = click_count,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                    );
                    page.evaluate(&code);
                }
            } else if event_type == "mouseReleased" {
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || globalThis.__obscura_click_target || document.activeElement || document.body;\
                            if (!target) return;\
                            var down = globalThis.__obscura_mouse_down;\
                            globalThis.__obscura_mouse_down = null;\
                            var evt = globalThis.__obscura_markTrusted(new MouseEvent('mouseup', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},button:{button_code},buttons:0,detail:{click_count},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            target.dispatchEvent(evt);\
                            if (!down || down.button !== {button_code} || {button_code} !== 0) return;\
                            var clickTarget = down.target;\
                            while (clickTarget && clickTarget !== target && !(clickTarget.contains && clickTarget.contains(target))) {{\
                                clickTarget = clickTarget.parentElement;\
                            }}\
                            if (!clickTarget) return;\
                            var tag = clickTarget.tagName;\
                            var type = (clickTarget.getAttribute && clickTarget.getAttribute('type') || '').toLowerCase();\
                            var checkable = tag === 'INPUT' && (type === 'checkbox' || type === 'radio');\
                            var oldChecked = checkable ? !!clickTarget.checked : false;\
                            var radioStates = null;\
                            if (checkable && type === 'radio') {{\
                                var radioName = clickTarget.getAttribute('name') || '';\
                                if (radioName) {{\
                                    var candidates = document.querySelectorAll('input');\
                                    radioStates = [];\
                                    for (var ri = 0; ri < candidates.length; ri++) {{\
                                        var radio = candidates[ri];\
                                        if ((radio.getAttribute('type') || '').toLowerCase() !== 'radio' || (radio.getAttribute('name') || '') !== radioName || radio.form !== clickTarget.form) continue;\
                                        radioStates.push([radio, !!radio.checked]);\
                                        if (radio !== clickTarget) radio.checked = false;\
                                    }}\
                                }}\
                                clickTarget.checked = true;\
                            }} else if (checkable) {{\
                                clickTarget.checked = !oldChecked;\
                            }}\
                            var click = globalThis.__obscura_markTrusted(new MouseEvent('click', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},button:0,buttons:0,detail:{click_count},altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            var cancelled = !clickTarget.dispatchEvent(click);\
                            if (cancelled) {{\
                                if (radioStates) {{\
                                    for (var rr = 0; rr < radioStates.length; rr++) radioStates[rr][0].checked = radioStates[rr][1];\
                                }} else if (checkable) clickTarget.checked = oldChecked;\
                                return;\
                            }}\
                            if (checkable && clickTarget.checked !== oldChecked) {{\
                                try {{ clickTarget.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {{bubbles:true}}))); }} catch(e) {{}}\
                                try {{ clickTarget.dispatchEvent(globalThis.__obscura_markTrusted(new Event('change', {{bubbles:true}}))); }} catch(e) {{}}\
                                return;\
                            }}\
                            var link = clickTarget.closest ? clickTarget.closest('a[href]') : null;\
                            if (!link && tag === 'A' && clickTarget.getAttribute('href')) link = clickTarget;\
                            if (link) {{\
                                var href = link.getAttribute('href');\
                                if (href && !href.startsWith('#') && !href.startsWith('javascript:')) location.assign(href);\
                            }} else if (tag === 'BUTTON' && type !== 'button' && type !== 'reset') {{\
                                var form = clickTarget.closest ? clickTarget.closest('form') : null;\
                                if (form) {{ try {{ if (typeof form.requestSubmit === 'function') {{ form.requestSubmit(clickTarget); }} else {{ form.submit(clickTarget); }} }} catch(e) {{}} }}\
                            }} else if (tag === 'INPUT' && (type === 'submit' || type === 'image')) {{\
                                var form2 = clickTarget.closest ? clickTarget.closest('form') : null;\
                                if (form2) {{ try {{ if (typeof form2.requestSubmit === 'function') {{ form2.requestSubmit(clickTarget); }} else {{ form2.submit(clickTarget); }} }} catch(e) {{}} }}\
                            }} else if ({click_count} >= 3 && (tag === 'INPUT' || tag === 'TEXTAREA')) {{\
                                var len = clickTarget.value ? clickTarget.value.length : 0;\
                                if (clickTarget.setSelectionRange) clickTarget.setSelectionRange(0, len);\
                                else {{ clickTarget.selectionStart = 0; clickTarget.selectionEnd = len; }}\
                            }}\
                        }})()",
                        x = x,
                        y = y,
                        button_code = button_code,
                        click_count = click_count,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                    );
                    page.evaluate(&code);
                    // A click that submits a form/location can wait out a full
                    // page Load (30s on challenge pages that never settle).
                    // The client already sees the dispatched DOM events, so
                    // cap the follow-through: 3s for the navigation itself,
                    // and never fail the click because it timed out.
                    let moved = match tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        page.process_pending_navigation(),
                    )
                    .await
                    {
                        Ok(Ok(moved)) => moved,
                        Ok(Err(_)) | Err(_) => false,
                    };
                    // Fork: a single page app answers a click by routing itself,
                    // with no document fetch. The client still has to be told the
                    // frame moved, or the click looks like it did nothing.
                    if moved {
                        let url = page.url_string();
                        let frame_id = page.frame_id.clone();
                        ctx.pending_events.push(crate::types::CdpEvent {
                            method: "Page.frameNavigated".into(),
                            params: json!({
                                "frame": {
                                    "id": frame_id,
                                    "url": url,
                                    "domainAndRegistry": "",
                                    "securityOrigin": "",
                                    "mimeType": "text/html",
                                    "adFrameStatus": { "adFrameType": "none" },
                                },
                                "type": "Navigation",
                            }),
                            session_id: Some(session_id.clone().unwrap_or_default()),
                        });
                    }
                }
            } else if event_type == "mouseMoved" {
                // Humanized trajectories (Go client) stream these ahead of a
                // click. Hover matters to detection scripts: dispatch a real
                // mousemove/pointermove pair at the element under the cursor,
                // mirroring the mousePressed elementFromPoint resolution.
                let seq = ctx.next_input_seq();
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || document.body;\
                            if (!target) return;\
                            globalThis.__obscura_click_target = target;\
                            try {{ globalThis.__obscura_engineInput({{kind:'pointermove', seq:{seq}, ts:performance.now(), prev:[]}}); }} catch(e) {{}}\
                            var mv = globalThis.__obscura_markTrusted(new MouseEvent('mousemove', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},button:0,buttons:0,detail:0,sourceCapabilities:new InputDeviceCapabilities({{firesTouchEvents:false}})}}));\
                            try {{ Object.defineProperty(mv, '_engineSeq', {{value:{seq}}}); }} catch(e) {{}}\
                            target.dispatchEvent(mv);\
                            if (typeof PointerEvent === 'function') {{\
                                var pv = globalThis.__obscura_markTrusted(new PointerEvent('pointermove', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},pointerId:1,pointerType:'mouse',isPrimary:true,width:1,height:1,pressure:0}}));\
                                try {{ Object.defineProperty(pv, '_engineSeq', {{value:{seq}}}); }} catch(e) {{}}\
                                target.dispatchEvent(pv);\
                            }}\
                        }})()",
                        x = x,
                        y = y,
                        seq = seq,
                    );
                    page.evaluate(&code);
                }
            } else if event_type == "mouseWheel" {
                let delta_x = params.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let delta_y = params.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    let code = format!(
                        "(function() {{\
                            var target = (document.elementFromPoint && document.elementFromPoint({x},{y})) || document.body || document.documentElement;\
                            if (!target) return;\
                            var wheel = globalThis.__obscura_markTrusted(new WheelEvent('wheel', {{bubbles:true,cancelable:true,view:globalThis,clientX:{x},clientY:{y},deltaX:{delta_x},deltaY:{delta_y},deltaMode:0,altKey:{alt_key},ctrlKey:{ctrl_key},metaKey:{meta_key},shiftKey:{shift_key}}}));\
                            if (!target.dispatchEvent(wheel)) return;\
                            var dx = {delta_x}, dy = {delta_y};\
                            var root = document.scrollingElement || document.documentElement || document.body;\
                            var scrollTarget = null;\
                            var el = target;\
                            while (el && el.nodeType === 1 && el !== root && el !== document.body && el !== document.documentElement) {{\
                                var maxX = Math.max(0, (el.scrollWidth || 0) - (el.clientWidth || 0));\
                                var maxY = Math.max(0, (el.scrollHeight || 0) - (el.clientHeight || 0));\
                                var style = null;\
                                try {{ style = getComputedStyle(el); }} catch (_e) {{}}\
                                var ox = style ? (style.overflowX || style.overflow || '') : '';\
                                var oy = style ? (style.overflowY || style.overflow || '') : '';\
                                var allowX = ox === 'auto' || ox === 'scroll' || ox === 'overlay';\
                                var allowY = oy === 'auto' || oy === 'scroll' || oy === 'overlay';\
                                var consumesX = allowX && ((dx > 0 && el.scrollLeft < maxX) || (dx < 0 && el.scrollLeft > 0));\
                                var consumesY = allowY && ((dy > 0 && el.scrollTop < maxY) || (dy < 0 && el.scrollTop > 0));\
                                if (consumesX || consumesY) {{ scrollTarget = el; break; }}\
                                el = el.parentElement;\
                            }}\
                            if (!scrollTarget) scrollTarget = root;\
                            if (scrollTarget === root && root && typeof root.scrollBy === 'function') {{\
                                var beforeX = root.scrollLeft, beforeY = root.scrollTop;\
                                root.scrollBy(dx, dy);\
                                if (root.scrollLeft !== beforeX || root.scrollTop !== beforeY) setTimeout(function() {{\
                                    try {{ document.dispatchEvent(new Event('scroll', {{bubbles:false}})); }} catch (_e) {{}}\
                                    try {{ globalThis.dispatchEvent(new Event('scroll', {{bubbles:false}})); }} catch (_e) {{}}\
                                }}, 0);\
                            }} else if (scrollTarget && typeof scrollTarget.scrollBy === 'function') scrollTarget.scrollBy(dx, dy);\
                        }})()",
                        x = x,
                        y = y,
                        delta_x = delta_x,
                        delta_y = delta_y,
                        alt_key = alt_key,
                        ctrl_key = ctrl_key,
                        meta_key = meta_key,
                        shift_key = shift_key,
                    );
                    page.evaluate(&code);
                }
            }

            Ok(json!({}))
        }
        "humanGesture" => {
            // Whole-trajectory hover: the client sends every point at once and
            // the page replays them on a setTimeout chain. Runtime.evaluate
            // would stall this behind its post-eval frame pump (1.5s+), so the
            // gesture runs through a direct page.evaluate with no pumping.
            let pts = params
                .get("points")
                .and_then(|v| v.as_array())
                .ok_or("points required")?;
            let mut coords = String::new();
            for p in pts {
                let x = p.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = p.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let d = p.get(2).and_then(|v| v.as_u64()).unwrap_or(10);
                coords.push_str(&format!("[{},{},{}],", x, y, d));
            }
            let press = params
                .get("press")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let press_delay = params
                .get("pressDelayMs")
                .and_then(|v| v.as_u64())
                .unwrap_or(80);
            let target_x = params
                .get("x")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let target_y = params
                .get("y")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let seq_base = ctx.next_input_seq();
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                // When `press` is set the gesture completes inside this one
                // evaluate: trajectory → mousedown → press gap → mouseup →
                // click. Runtime.evaluate would stall each of those behind
                // its frame pump (1.5s+ on busy pages), so interactive
                // gestures must not go through it.
                let click_seq = if press {
                    format!(
                        "function fire() {{\
                            var x = {target_x}, y = {target_y};\
                            var target = (document.elementFromPoint && document.elementFromPoint(x, y)) || globalThis.__obscura_click_target || document.body;\
                            globalThis.__obscura_click_target = target;\
                            seq++;\
                            try {{ globalThis.__obscura_engineInput({{kind:'pointerdown', seq:seq, ts:performance.now(), prev:[]}}); }} catch(e) {{}}\
                            var pd = globalThis.__obscura_markTrusted(new PointerEvent('pointerdown', {{bubbles:true,cancelable:true,view:globalThis,clientX:x,clientY:y,pointerId:1,pointerType:'mouse',isPrimary:true,width:1,height:1,pressure:0.5,buttons:1}}));\
                            var md = globalThis.__obscura_markTrusted(new MouseEvent('mousedown', {{bubbles:true,cancelable:true,view:globalThis,clientX:x,clientY:y,button:0,buttons:1,detail:1,sourceCapabilities:new InputDeviceCapabilities({{firesTouchEvents:false}})}}));\
                            try {{ Object.defineProperty(pd, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                            try {{ Object.defineProperty(md, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                            target.dispatchEvent(pd);\
                            target.dispatchEvent(md);\
                            setTimeout(function() {{\
                                seq++;\
                                try {{ globalThis.__obscura_engineInput({{kind:'pointerup', seq:seq, ts:performance.now(), prev:[]}}); }} catch(e) {{}}\
                                var pu = globalThis.__obscura_markTrusted(new PointerEvent('pointerup', {{bubbles:true,cancelable:true,view:globalThis,clientX:x,clientY:y,pointerId:1,pointerType:'mouse',isPrimary:true,width:1,height:1,pressure:0,buttons:0}}));\
                                var mu = globalThis.__obscura_markTrusted(new MouseEvent('mouseup', {{bubbles:true,cancelable:true,view:globalThis,clientX:x,clientY:y,button:0,buttons:0,detail:1,sourceCapabilities:new InputDeviceCapabilities({{firesTouchEvents:false}})}}));\
                                var ck = globalThis.__obscura_markTrusted(new MouseEvent('click', {{bubbles:true,cancelable:true,view:globalThis,clientX:x,clientY:y,button:0,buttons:0,detail:1,sourceCapabilities:new InputDeviceCapabilities({{firesTouchEvents:false}})}}));\
                                try {{ Object.defineProperty(pu, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                                try {{ Object.defineProperty(mu, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                                try {{ Object.defineProperty(ck, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                                target.dispatchEvent(pu);\
                                target.dispatchEvent(mu);\
                                target.dispatchEvent(ck);\
                            }}, {press_delay});\
                        }}\
                        setTimeout(fire, 60);"
                    )
                } else {
                    String::new()
                };
                // Per-step engine input metadata: seq increments per dispatched
                // event, moves carry the raw samples since the previous dispatch
                // as their coalesced history (like a real browser process batch).
                let code = format!(
                    "(function() {{\
                        var pts = [{coords}];\
                        var i = 0;\
                        var seq = {seq_base};\
                        var prevPts = [];\
                        function step() {{\
                            if (i >= pts.length) {{ {click_seq} return; }}\
                            var p = pts[i++];\
                            var target = (document.elementFromPoint && document.elementFromPoint(p[0], p[1])) || document.body;\
                            if (target) {{\
                                globalThis.__obscura_click_target = target;\
                                seq++;\
                                try {{ globalThis.__obscura_engineInput({{kind:'pointermove', seq:seq, ts:performance.now(), prev:prevPts}}); }} catch(e) {{}}\
                                var pm = globalThis.__obscura_markTrusted(new PointerEvent('pointermove', {{bubbles:true,cancelable:true,view:globalThis,clientX:p[0],clientY:p[1],pointerId:1,pointerType:'mouse',isPrimary:true,width:1,height:1,pressure:0}}));\
                                try {{ Object.defineProperty(pm, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                                var mm = globalThis.__obscura_markTrusted(new MouseEvent('mousemove', {{bubbles:true,cancelable:true,view:globalThis,clientX:p[0],clientY:p[1],button:0,buttons:0,detail:0,sourceCapabilities:new InputDeviceCapabilities({{firesTouchEvents:false}})}}));\
                                try {{ Object.defineProperty(mm, '_engineSeq', {{value:seq}}); }} catch(e) {{}}\
                                try {{\
                                    target.dispatchEvent(pm);\
                                    target.dispatchEvent(mm);\
                                }} catch (e) {{}}\
                                prevPts.push([p[0], p[1], performance.now() - p[2]]);\
                                if (prevPts.length > 6) prevPts.shift();\
                            }}\
                            setTimeout(step, p[2]);\
                        }}\
                        step();\
                    }})()",
                    seq_base = seq_base,
                );
                page.evaluate(&code);
                // The press sequence runs on a setTimeout chain — someone has
                // to drive the event loop for it to fire, and the client
                // already treats this call as asynchronous. Pump until the
                // gesture completes (bounded), so the click actually lands
                // instead of starving until the next unrelated command.
                let pump_budget = std::time::Duration::from_millis(press_delay + 700);
                if let Ok(budget) = std::time::Duration::try_from(pump_budget) {
                    let _ = tokio::time::timeout(budget, async {
                        if let Some(page) = ctx.get_session_page_mut(session_id) {
                            page.settle(press_delay + 650).await;
                        }
                    })
                    .await;
                }
            }
            Ok(json!({}))
        }
        "dispatchKeyEvent" => {
            let event_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let code = params.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

            if let Some(page) = ctx.get_session_page_mut(session_id) {
                match event_type {
                    "keyDown" | "rawKeyDown" => {
                        let js = format!(
                            "(function() {{\
                                var target = document.activeElement || document.body;\
                                var evt = globalThis.__obscura_markTrusted(new KeyboardEvent('keydown', {{bubbles:true,cancelable:true,key:'{key}',code:'{code}'}}));\
                                target.dispatchEvent(evt);\
                            }})()",
                            // Escape backslash BEFORE single-quote (as the text
                            // path below does) so a key like "\" — Chrome's
                            // backslash key — doesn't escape the closing quote
                            // and produce a syntax error that drops the event.
                            key = key.replace('\\', "\\\\").replace('\'', "\\'"),
                            code = code.replace('\\', "\\\\").replace('\'', "\\'"),
                        );
                        page.evaluate(&js);

                        if !text.is_empty() && text != "\r" && text != "\n" {
                            page.evaluate(&insert_text_js(text));
                        }

                        if key == "Enter" {
                            // In a textarea Enter inserts a newline; in input fields
                            // it submits the containing form. Real Chrome distinguishes
                            // these two and we should too: previously every Enter tried
                            // to submit the nearest form even from a textarea.
                            let js = "(function() {\
                                var target = document.activeElement;\
                                if (!target) return;\
                                target.dispatchEvent(globalThis.__obscura_markTrusted(new KeyboardEvent('keypress', {bubbles:true,key:'Enter',code:'Enter'})));\
                                if (target.localName === 'textarea') {\
                                    globalThis.__obscura_setFieldValue(target, 'value', (target.value || '') + '\\n');\
                                    target.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {bubbles:true})));\
                                } else {\
                                    var form = target.form || (target.closest && target.closest('form'));\
                                    if (form) {{ try {{ if (typeof form.requestSubmit === 'function') {{ form.requestSubmit(); }} else {{ form.submit(); }} }} catch(e) {{}} }}\
                                }\
                            })()";
                            page.evaluate(js);
                        }

                        if key == "Backspace" {
                            page.evaluate(BACKSPACE_JS);
                        }
                    }
                    "keyUp" => {
                        let js = format!(
                            "(function() {{\
                                var target = document.activeElement || document.body;\
                                var evt = globalThis.__obscura_markTrusted(new KeyboardEvent('keyup', {{bubbles:true,key:'{key}',code:'{code}'}}));\
                                target.dispatchEvent(evt);\
                            }})()",
                            key = key.replace('\\', "\\\\").replace('\'', "\\'"),
                            code = code.replace('\\', "\\\\").replace('\'', "\\'"),
                        );
                        page.evaluate(&js);
                    }
                    "char" => {
                        if !text.is_empty() {
                            page.evaluate(&insert_text_js(text));
                            // Pump event loop so Angular change detection picks up the input
                            page.settle(50).await;
                        }
                    }
                    _ => {}
                }
            }

            Ok(json!({}))
        }
        "dispatchTouchEvent" => Ok(json!({})),
        "setIgnoreInputEvents" => Ok(json!({})),
        _ => Err(format!("Unknown Input method: {}", method)),
    }
}
