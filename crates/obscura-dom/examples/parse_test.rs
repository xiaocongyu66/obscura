use obscura_dom::{parse_html, DomTree};
fn main() {
    let tree: DomTree = parse_html("<html><body><div id=\"x\"><span></span></div><a href=\"#\"></a></body></html>");
    for sel in ["#x::before", "#x::selection", "#x:first-line", ":target", ":lang(en)", "#x::after"] {
        let r = tree.query_selector_all(sel).map(|v| v.len()).unwrap_or(9999);
        println!("{} => {:?}", sel, r);
    }
}
