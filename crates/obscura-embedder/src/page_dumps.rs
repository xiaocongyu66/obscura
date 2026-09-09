//! CLI-equivalent page dumps over the headless Servo kernel.
//!
//! Each dump format that used to walk the self-engine DOM tree now runs as
//! one `evaluate_sync` expression — the kernel's own DOM is the source of
//! truth, so no tree/porting layer is needed.

use crate::HeadlessServo;
use std::time::Duration;

/// JS expression that walks `document.body` and returns a markdown string.
/// Carried over verbatim from the self-engine's shared extraction script —
/// it is plain browser-side JS with no engine-specific dependencies.
pub const HTML_TO_MARKDOWN_JS: &str = r#"
(function() {
    function toMd(el, depth) {
        if (!el) return '';
        var out = '';
        if (el.nodeType === 3) return el.textContent || '';
        if (el.nodeType !== 1) return '';
        var tag = (el.tagName || '').toLowerCase();
        var children = '';
        var cn = el.childNodes || [];
        for (var i = 0; i < cn.length; i++) children += toMd(cn[i], depth);
        children = children.replace(/\n{3,}/g, '\n\n');
        switch(tag) {
            case 'h1': return '\n# ' + children.trim() + '\n\n';
            case 'h2': return '\n## ' + children.trim() + '\n\n';
            case 'h3': return '\n### ' + children.trim() + '\n\n';
            case 'h4': return '\n#### ' + children.trim() + '\n\n';
            case 'h5': return '\n##### ' + children.trim() + '\n\n';
            case 'h6': return '\n###### ' + children.trim() + '\n\n';
            case 'p': return '\n' + children.trim() + '\n\n';
            case 'br': return '\n';
            case 'hr': return '\n---\n\n';
            case 'strong': case 'b': return '**' + children + '**';
            case 'em': case 'i': return '*' + children + '*';
            case 'code': return '`' + children + '`';
            case 'pre': return '\n```\n' + children + '\n```\n\n';
            case 'blockquote': return '\n> ' + children.trim().replace(/\n/g, '\n> ') + '\n\n';
            case 'a':
                var href = el.getAttribute('href') || '';
                if (href && children.trim()) return '[' + children.trim() + '](' + href + ')';
                return children;
            case 'img':
                var src = el.getAttribute('src') || '';
                var alt = el.getAttribute('alt') || '';
                return '![' + alt + '](' + src + ')';
            case 'ul': case 'ol':
                return '\n' + children + '\n';
            case 'li':
                var parent = el.parentNode;
                var isOrdered = parent && parent.tagName && parent.tagName.toLowerCase() === 'ol';
                var bullet = isOrdered ? '1. ' : '- ';
                return bullet + children.trim() + '\n';
            case 'table': return '\n' + children + '\n';
            case 'thead': case 'tbody': case 'tfoot': return children;
            case 'tr':
                var cells = [];
                var tds = el.childNodes || [];
                for (var j = 0; j < tds.length; j++) {
                    if (tds[j].nodeType === 1) cells.push(toMd(tds[j], depth).trim());
                }
                return '| ' + cells.join(' | ') + ' |\n';
            case 'th': case 'td': return children;
            case 'script': case 'style': case 'noscript': case 'link': case 'meta': return '';
            case 'div': case 'section': case 'article': case 'main': case 'aside': case 'nav': case 'header': case 'footer':
            default: return children;
        }
    }
    return toMd(document.body, 0).trim();
})()
"#;

const EVAL_TIMEOUT: Duration = Duration::from_secs(30);

/// `document.documentElement.outerHTML` — the serialized live DOM.
pub fn dump_html(servo: &HeadlessServo) -> Result<String, String> {
    servo.evaluate_sync("document.documentElement.outerHTML", EVAL_TIMEOUT)
}

/// Visible text content, whitespace-normalized the way the CLI did it.
pub fn dump_text(servo: &HeadlessServo) -> Result<String, String> {
    let raw = servo.evaluate_sync(
        "(document.body ? document.body.innerText : document.documentElement.textContent)",
        EVAL_TIMEOUT,
    )?;
    Ok(raw)
}

/// Every anchor href on the page, deduplicated, resolved against the base URL.
pub fn dump_links(servo: &HeadlessServo) -> Result<String, String> {
    servo.evaluate_sync(
        r#"(function() {
            var out = [];
            var seen = {};
            var links = document.querySelectorAll('a[href]');
            for (var i = 0; i < links.length; i++) {
                var href = links[i].href; // property resolves against document.baseURI
                if (href && !seen[href]) { seen[href] = 1; out.push(href); }
            }
            return out.join('\n');
        })()"#,
        EVAL_TIMEOUT,
    )
}

/// Markdown conversion of the rendered page body.
pub fn dump_markdown(servo: &HeadlessServo) -> Result<String, String> {
    servo.evaluate_sync(HTML_TO_MARKDOWN_JS, EVAL_TIMEOUT)
}

/// One URL per line for every sub-resource the rendered page references —
/// the same attribute set the self-engine's asset walker covered.
pub fn dump_assets(servo: &HeadlessServo) -> Result<String, String> {
    servo.evaluate_sync(
        r#"(function() {
            function resolve(el, attr) {
                var v = el.getAttribute(attr);
                return v ? el[attr] || new URL(v, document.baseURI).href : null;
            }
            var out = [];
            var seen = {};
            function push(v) { if (v && !seen[v]) { seen[v] = 1; out.push(v); } }
            var sel = 'script[src], link[href], img[src], iframe[src], ' +
                      'audio[src], video[src], source[src], embed[src], object[data]';
            var nodes = document.querySelectorAll(sel);
            for (var i = 0; i < nodes.length; i++) {
                var el = nodes[i];
                var tag = el.tagName.toLowerCase();
                if (tag === 'object') push(resolve(el, 'data'));
                else if (tag === 'link') push(resolve(el, 'href'));
                else push(resolve(el, 'src'));
            }
            return out.join('\n');
        })()"#,
        EVAL_TIMEOUT,
    )
}

/// All cookies (HttpOnly included) as a JSON array, via the kernel's
/// document.cookie — Servo exposes HttpOnly exclusion the standard way;
/// the challenge-token use case reads what the page itself can read.
pub fn dump_cookies(servo: &HeadlessServo) -> Result<String, String> {
    servo.evaluate_sync("document.cookie", EVAL_TIMEOUT).map(|raw| {
        let pairs: Vec<String> = raw
            .split("; ")
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                format!("{{\"name\":{},\"value\":{}}}", json_str(k), json_str(v))
            })
            .collect();
        format!("[{}]", pairs.join(","))
    })
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
