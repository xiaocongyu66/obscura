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


    pub fn evaluate(&mut self, expression: &str) -> Result<serde_json::Value, String> {
        self.begin_javascript_task();
        let wrapped = Self::wrap_expression(expression);
        let result = self
            .execute_runtime_script("<eval>", wrapped)
            .map_err(|e| format!("JS error: {}", e))?;
        self.v8_to_json(result)
    }

    pub async fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.evaluate_for_cdp_with_timeout(
            expression,
            return_by_value,
            await_promise,
            DEFAULT_CDP_AWAIT_TIMEOUT_MS,
        )
        .await
    }

    pub async fn evaluate_for_cdp_with_timeout(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
        await_timeout_ms: u64,
    ) -> Result<RemoteObjectInfo, String> {
        if !await_promise && return_by_value {
            let val = self.evaluate(expression)?;
            return Ok(Self::info_from_json(&val));
        }
        self.begin_javascript_task();

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        // Same trailing-semicolon trim as wrap_expression — Playwright's
        // utility-script eval ends with `})();`, and `({expr})` would
        // otherwise become `(...;)` which is a parse-time SyntaxError.
        let cleaned_expr = expression
            .trim()
            .trim_end_matches(|c: char| c == ';' || c.is_whitespace());

        // Puppeteer / Playwright bundles end with a `//# sourceURL=...`
        // line comment. If we put `{expr})` on a single line the comment
        // swallows the closing paren and our wrapper breaks. A newline
        // before the `)` terminates any trailing line comment so the
        // parens close on their own line.
        let done_counter = self.object_counter;
        let meta_code = if await_promise {
            format!(
                "(async function() {{\n\
                    try {{\n\
                        var __result = await (\n{expr}\n);\n\
                        globalThis.__obscura_objects['{oid}'] = __result;\n\
                        globalThis.__obscura_await_meta = {meta_fn};\n\
                        globalThis.__obscura_await_rejected = false;\n\
                    }} catch(e) {{\n\
                        globalThis.__obscura_objects['{oid}'] = e;\n\
                        globalThis.__obscura_await_meta = {err_meta_fn};\n\
                        globalThis.__obscura_await_rejected = true;\n\
                    }}\n\
                    globalThis.__obscura_done_{done_counter} = true;\n\
                }})()",
                expr = cleaned_expr,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
                err_meta_fn = Self::meta_extract_js("e"),
                done_counter = done_counter,
            )
        } else {
            format!(
                "(function() {{\n\
                    var __result;\n\
                    try {{ __result = (\n{expr}\n); }} catch(e) {{ __result = undefined; }}\n\
                    globalThis.__obscura_objects['{oid}'] = __result;\n\
                    return {meta_fn};\n\
                }})()",
                expr = cleaned_expr,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
            )
        };

        let result = self
            .execute_runtime_script("<eval-remote>", meta_code)
            .map_err(|e| format!("JS error: {}", e))?;

        let meta_str = if await_promise {
            let __t0 = std::time::Instant::now();
            let sentinel = format!("globalThis.__obscura_done_{done_counter} === true");
            let settled = self
                .resolve_promises_until(
                    |rt| {
                        rt.execute_runtime_script("<done?>", sentinel.clone())
                            .ok()
                            .and_then(|v| rt.v8_to_json(v).ok())
                            .and_then(|j| j.as_bool())
                            .unwrap_or(false)
                    },
                    await_timeout_ms,
                )
                .await;
            if !settled {
                return Err(format!(
                    "Runtime.evaluate promise did not settle within {await_timeout_ms}ms"
                ));
            }
            let __dt = __t0.elapsed();
            if __dt > std::time::Duration::from_secs(1) {
                let preview: String = expression
                    .chars()
                    .take(200)
                    .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                    .collect();
                tracing::debug!(
                    "Runtime.evaluate awaitPromise took {}ms; expr={}",
                    __dt.as_millis(),
                    preview,
                );
            }
            let rejected = self
                .execute_runtime_script(
                    "<readRejected>",
                    "globalThis.__obscura_await_rejected".to_string(),
                )
                .map_err(|e| format!("JS error: {}", e))?;
            if self.v8_to_json(rejected)?.as_bool().unwrap_or(false) {
                let err = self.execute_runtime_script("<readError>", format!("String(globalThis.__obscura_objects['{0}'] && (globalThis.__obscura_objects['{0}'].message || globalThis.__obscura_objects['{0}']))", oid))
                    .map_err(|e| format!("JS error: {}", e))?;
                return Err(format!(
                    "Promise rejected: {}",
                    self.v8_to_json(err)?.as_str().unwrap_or("")
                ));
            }
            self.execute_runtime_script("<readMeta>", "globalThis.__obscura_await_meta".to_string())
                .map_err(|e| format!("JS error: {}", e))?
        } else {
            result
        };
        let meta_str = self.v8_to_json(meta_str)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str)
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );

        if await_promise && return_by_value {
            let read = self
                .execute_runtime_script(
                    "<readResult>",
                    format!("globalThis.__obscura_objects['{}']", oid),
                )
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(read)?;
            return Ok(Self::info_from_json(&json_val));
        }

        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub async fn call_function_on_for_cdp(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.call_function_on_for_cdp_with_timeout(
            function_declaration,
            object_id,
            arguments,
            return_by_value,
            await_promise,
            DEFAULT_CDP_AWAIT_TIMEOUT_MS,
        )
        .await
    }

    pub async fn call_function_on_for_cdp_with_timeout(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
        await_timeout_ms: u64,
    ) -> Result<RemoteObjectInfo, String> {
        self.begin_javascript_task();
        let this_expr = self.resolve_this(object_id);
        let (setup, args_list) = self.build_args(arguments);

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        if await_promise {
            let done_counter = self.object_counter;
            let err_meta_fn = Self::meta_extract_js("__result");
            let code = format!(
                "(async function() {{\n\
                    {setup}\n\
                    var __fn = ({fn_decl});\n\
                    var __this = ({this_expr});\n\
                    var __result;\n\
                    try {{\n\
                        __result = await __fn.call(__this, {args});\n\
                        globalThis.__obscura_objects['{oid}'] = __result;\n\
                        globalThis.__obscura_await_meta = {meta_fn};\n\
                    }} catch(e) {{\n\
                        __result = e;\n\
                        globalThis.__obscura_objects['{oid}'] = e;\n\
                        globalThis.__obscura_await_meta = {err_meta_fn};\n\
                    }} finally {{\n\
                        globalThis.__obscura_done_{done_counter} = true;\n\
                    }}\n\
                }})()",
                setup = setup,
                fn_decl = function_declaration,
                this_expr = this_expr,
                args = args_list,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
                err_meta_fn = err_meta_fn,
                done_counter = done_counter,
            );

            self.execute_runtime_script("<callFnAsync>", code)
                .map_err(|e| format!("JS error: {}", e))?;

            let __t0 = std::time::Instant::now();
            let sentinel = format!("globalThis.__obscura_done_{done_counter} === true");
            let settled = self
                .resolve_promises_until(
                    |rt| {
                        rt.execute_runtime_script("<done?>", sentinel.clone())
                            .ok()
                            .and_then(|v| rt.v8_to_json(v).ok())
                            .and_then(|j| j.as_bool())
                            .unwrap_or(false)
                    },
                    await_timeout_ms,
                )
                .await;
            if !settled {
                return Err(format!(
                    "Runtime.callFunctionOn promise did not settle within {await_timeout_ms}ms"
                ));
            }
            let __dt = __t0.elapsed();
            if __dt > std::time::Duration::from_secs(1) {
                let preview: String = function_declaration
                    .chars()
                    .take(300)
                    .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                    .collect();
                tracing::debug!(
                    "Runtime.callFunctionOn awaitPromise took {}ms; fn={}",
                    __dt.as_millis(),
                    preview,
                );
            }

            if return_by_value {
                let read = self
                    .execute_runtime_script(
                        "<readResult>",
                        format!("globalThis.__obscura_objects['{}']", oid),
                    )
                    .map_err(|e| format!("JS error: {}", e))?;
                let json_val = self.v8_to_json(read)?;
                return Ok(Self::info_from_json(&json_val));
            }

            let meta_result = self
                .execute_runtime_script("<readMeta>", "globalThis.__obscura_await_meta".to_string())
                .map_err(|e| format!("JS error: {}", e))?;
            let meta_str = self.v8_to_json(meta_result)?;
            let meta_json = if let serde_json::Value::String(s) = &meta_str {
                serde_json::from_str(s).unwrap_or(meta_str.clone())
            } else {
                meta_str
            };
            self.object_store.insert(
                oid.clone(),
                format!("globalThis.__obscura_objects['{}']", oid),
            );
            return Ok(Self::info_from_meta(&meta_json, Some(oid)));
        }

        if return_by_value {
            let code = format!(
                "(function() {{\n\
                    {setup}\n\
                    var __fn = ({fn_decl});\n\
                    var __this = ({this_expr});\n\
                    return __fn.call(__this, {args});\n\
                }})()",
                setup = setup,
                fn_decl = function_declaration,
                this_expr = this_expr,
                args = args_list,
            );
            let result = self
                .execute_runtime_script("<callFnByValue>", code)
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(result)?;
            return Ok(Self::info_from_json(&json_val));
        }

        let code = format!(
            "(function() {{\n\
                {setup}\n\
                var __fn = ({fn_decl});\n\
                var __this = ({this_expr});\n\
                var __result = __fn.call(__this, {args});\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            setup = setup,
            fn_decl = function_declaration,
            this_expr = this_expr,
            args = args_list,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .execute_runtime_script("<callFnRemote>", code)
            .map_err(|e| format!("JS error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }
    pub async fn call_function_on(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.call_function_on_for_cdp(
            function_declaration,
            object_id,
            arguments,
            return_by_value,
            false,
        )
        .await
    }
    pub fn store_object(&mut self, js_expression: &str) -> Result<String, String> {
        self.begin_javascript_task();
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "globalThis.__obscura_objects['{}'] = ({});",
            oid, js_expression,
        );
        self.execute_runtime_script("<store>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(oid)
    }

    pub fn store_object_with_meta(
        &mut self,
        js_expression: &str,
    ) -> Result<RemoteObjectInfo, String> {
        self.begin_javascript_task();
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "(function() {{\n\
                var __result = (\n{expr}\n);\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            expr = js_expression,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .execute_runtime_script("<store-meta>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub fn release_object(&mut self, object_id: &str) {
        if self.object_store.remove(object_id).is_some() {
            let code = format!("delete globalThis.__obscura_objects['{}'];", object_id,);
            let _ = self.execute_runtime_script("<release>", code);
        }
    }

    pub fn release_object_group(&mut self) {
        let _ = self.execute_runtime_script(
            "<releaseGroup>",
            "globalThis.__obscura_objects = {};".to_string(),
        );
        self.object_store.clear();
    }

    pub(crate) fn make_oid(&self, counter: u64) -> String {
        format!("{{\"injectedScriptId\":1,\"id\":{}}}", counter)
    }

    pub(crate) fn wrap_expression(expression: &str) -> String {
        let trimmed = expression.trim();

        let is_multi_statement = trimmed.starts_with("var ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("if ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("return ");

        if is_multi_statement {
            format!(
                "(function() {{ try {{\n{}\n}} catch(e) {{ return null; }} }})()",
                expression
            )
        } else {
            // Strip trailing semicolons + whitespace before wrapping in
            // `return (...);`. Playwright's utility-script expression is
            // an IIFE that ends with `})();` — leaving the `;` in place
            // produces `return (...;);`, a SyntaxError. The script fails
            // to parse, the catch never fires (parse errors are not
            // catchable), and the function silently returns `undefined`.
            // Stripping makes the wrapped expression syntactically valid.
            //
            // The newline before the trailing `)` also terminates any
            // `//# sourceURL=...` line comment the caller may have appended
            // (Puppeteer's evaluated bundles do).
            let cleaned = trimmed.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
            format!(
                "(function() {{ try {{ return (\n{}\n); }} catch(e) {{ return null; }} }})()",
                cleaned
            )
        }
    }

    pub(crate) fn meta_extract_js(var_name: &str) -> String {
        format!(
            r#"(function(v) {{
                var t = typeof v;
                var st = null, cn = '', desc = '';
                if (v === null) {{ t = 'object'; st = 'null'; }}
                else if (v === undefined) {{ t = 'undefined'; }}
                else if (Array.isArray(v)) {{
                    st = 'array'; cn = 'Array';
                    desc = 'Array(' + v.length + ')';
                }}
                else if (t === 'object' && typeof v._nid === 'number') {{
                    st = 'node';
                    cn = v.constructor ? v.constructor.name : 'Node';
                    if (v.nodeType === 9) cn = 'HTMLDocument';
                    else if (v.nodeType === 1) cn = 'HTML' + (v.tagName || 'Element').charAt(0) + (v.tagName || 'Element').slice(1).toLowerCase() + 'Element';
                    desc = v.tagName ? v.tagName.toLowerCase() : (v.nodeName || 'node');
                }}
                else if (t === 'function') {{
                    cn = 'Function';
                    desc = v.name ? 'function ' + v.name + '()' : 'function()';
                }}
                else if (t === 'object') {{
                    cn = (v.constructor && v.constructor.name) || 'Object';
                    desc = cn;
                }}
                else {{ desc = String(v); }}
                return JSON.stringify({{type:t,subtype:st,className:cn,description:desc}});
            }})({var_name})"#,
            var_name = var_name,
        )
    }

    pub(crate) fn resolve_this(&self, object_id: Option<&str>) -> String {
        match object_id {
            Some(oid) => {
                if let Some(retrieval) = self.object_store.get(oid) {
                    retrieval.clone()
                } else if oid.starts_with("node-") {
                    let nid = oid.strip_prefix("node-").unwrap_or("0");
                    format!(
                        "(function() {{ \
                            var nid = {}; \
                            var cache = globalThis._cache || new Map(); \
                            if (cache.has(nid)) return cache.get(nid); \
                            return null; \
                        }})()",
                        nid
                    )
                } else {
                    "globalThis".to_string()
                }
            }
            None => "globalThis".to_string(),
        }
    }

    pub(crate) fn build_args(&self, arguments: &[serde_json::Value]) -> (String, String) {
        let mut setup_lines = Vec::new();
        let mut arg_names = Vec::new();

        for (i, arg) in arguments.iter().enumerate() {
            let arg_name = format!("__arg{}", i);
            if let Some(value) = arg.get("value") {
                let json_str =
                    serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_string());
                setup_lines.push(format!("var {} = {};", arg_name, json_str));
            } else if let Some(oid) = arg.get("objectId").and_then(|v| v.as_str()) {
                if let Some(retrieval) = self.object_store.get(oid) {
                    setup_lines.push(format!("var {} = {};", arg_name, retrieval));
                } else {
                    setup_lines.push(format!("var {} = undefined;", arg_name));
                }
            } else if let Some(unser) = arg.get("unserializableValue").and_then(|v| v.as_str()) {
                setup_lines.push(format!("var {} = {};", arg_name, unser));
            } else {
                setup_lines.push(format!("var {} = undefined;", arg_name));
            }
            arg_names.push(arg_name);
        }

        (setup_lines.join("\n"), arg_names.join(", "))
    }

    pub(crate) fn v8_to_json(
        &mut self,
        result: deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<serde_json::Value, String> {
        let scope = &mut self.runtime.handle_scope();
        let local = deno_core::v8::Local::new(scope, result);

        if local.is_undefined() || local.is_null() {
            return Ok(serde_json::Value::Null);
        }
        if local.is_boolean() {
            return Ok(serde_json::Value::Bool(local.boolean_value(scope)));
        }
        if local.is_number() {
            let n = local.number_value(scope).unwrap_or(0.0);
            return Ok(serde_json::json!(n));
        }
        if local.is_string() {
            let s = local.to_rust_string_lossy(scope);
            return Ok(serde_json::Value::String(s));
        }

        let global = scope.get_current_context().global(scope);
        let json_obj_str = deno_core::v8::String::new(scope, "JSON").unwrap();
        if let Some(json_obj) = global.get(scope, json_obj_str.into()) {
            if let Some(json_obj) = json_obj.to_object(scope) {
                let stringify_str = deno_core::v8::String::new(scope, "stringify").unwrap();
                if let Some(stringify_fn) = json_obj.get(scope, stringify_str.into()) {
                    if let Ok(stringify_fn) =
                        deno_core::v8::Local::<deno_core::v8::Function>::try_from(stringify_fn)
                    {
                        let args = [local];
                        if let Some(result) = stringify_fn.call(scope, json_obj.into(), &args) {
                            let json_str = result.to_rust_string_lossy(scope);
                            if let Ok(val) = serde_json::from_str(&json_str) {
                                return Ok(val);
                            }
                        }
                    }
                }
            }
        }

        let s = local.to_rust_string_lossy(scope);
        Ok(serde_json::Value::String(s))
    }

    pub(crate) fn info_from_json(value: &serde_json::Value) -> RemoteObjectInfo {
        match value {
            serde_json::Value::Null => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("null".into()),
                class_name: String::new(),
                description: "null".into(),
                object_id: None,
                value: Some(serde_json::Value::Null),
            },
            serde_json::Value::Bool(b) => RemoteObjectInfo {
                js_type: "boolean".into(),
                subtype: None,
                class_name: String::new(),
                description: b.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Number(n) => RemoteObjectInfo {
                js_type: "number".into(),
                subtype: None,
                class_name: String::new(),
                description: n.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::String(s) => RemoteObjectInfo {
                js_type: "string".into(),
                subtype: None,
                class_name: String::new(),
                description: s.clone(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Array(arr) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("array".into()),
                class_name: "Array".into(),
                description: format!("Array({})", arr.len()),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Object(_) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: None,
                class_name: "Object".into(),
                description: "Object".into(),
                object_id: None,
                value: Some(value.clone()),
            },
        }
    }

    pub(crate) fn info_from_meta(meta: &serde_json::Value, object_id: Option<String>) -> RemoteObjectInfo {
        let js_type = meta
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("undefined")
            .to_string();
        let subtype = meta
            .get("subtype")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let class_name = meta
            .get("className")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let description = meta
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let value = if js_type != "object" && js_type != "function" {
            meta.get("description")
                .and_then(|v| v.as_str())
                .map(|s| serde_json::Value::String(s.to_string()))
        } else {
            None
        };

        RemoteObjectInfo {
            js_type,
            subtype,
            class_name,
            description,
            object_id,
            value,
        }
    }

// IMPL-WRAP-END
}
