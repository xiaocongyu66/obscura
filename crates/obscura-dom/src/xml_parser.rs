//! XML / XHTML parser — uses xmloxide to parse XML documents and build
//! an obscura DomTree.
//!
//! XHTML documents (`Content-Type: application/xhtml+xml`) and XML
//! documents served as `text/xml` / `application/xml` use this parser
//! instead of the HTML5 tree builder. XML is strict (well-formedness
//! errors stop parsing), while HTML5 is permissive.
//!
//! The parser:
//! 1. Calls `xmloxide::Document::parse_str(src)` to build an xmloxide tree
//! 2. Walks the xmloxide tree and creates equivalent obscura DomTree nodes
//! 3. Returns a DomTree that the rest of obscura can use transparently

use crate::tree::{DomTree, NodeData, NodeId};
use html5ever::{LocalName, Namespace, QualName};
use xmloxide::tree::{Document as XmlDoc, NodeId as XmlId, NodeKind};

/// Parse an XML / XHTML document string into an obscura DomTree.
///
/// On parse error, returns a DomTree with just a document node (empty),
/// matching how browsers show a parse-error page.
pub fn parse_xml(src: &str) -> DomTree {
    let tree = DomTree::new();
    let doc_id = tree.document();

    match XmlDoc::parse_str(src) {
        Ok(xml_doc) => {
            let root = xml_doc.root();
            convert_node(&xml_doc, &tree, root, doc_id);
        }
        Err(e) => {
            tracing::warn!("XML parse error: {:?}", e);
        }
    }
    tree
}

/// Recursively convert an xmloxide node (and its subtree) into obscura
/// DomTree nodes, appending them to `parent_id`.
fn convert_node(xml_doc: &XmlDoc, tree: &DomTree, xml_id: XmlId, parent_id: NodeId) {
    let node = xml_doc.node(xml_id);
    match &node.kind {
        NodeKind::Document => {
            // Skip the document node itself — the obscura DomTree already
            // has one. Just recurse into children.
            for child_id in xml_doc.children(xml_id) {
                convert_node(xml_doc, tree, child_id, parent_id);
            }
        }
        NodeKind::Element { name, attributes, .. } => {
            // Create the element node.
            let qual_name = QualName::new(
                None,
                Namespace::from("http://www.w3.org/1999/xhtml"),
                LocalName::from(name.as_str()),
            );
            let obscura_attrs: Vec<crate::tree::Attribute> = attributes
                .iter()
                .map(|attr| crate::tree::Attribute {
                    name: QualName::new(None, ns!(), LocalName::from(attr.name.as_str())),
                    value: attr.value.clone(),
                })
                .collect();

            let child_id = tree.new_node(NodeData::Element {
                name: qual_name,
                attrs: obscura_attrs,
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            });
            tree.append_child(parent_id, child_id);

            // Recurse into children.
            for grandchild in xml_doc.children(xml_id) {
                convert_node(xml_doc, tree, grandchild, child_id);
            }
        }
        NodeKind::Text { content } | NodeKind::CData { content } => {
            let text_id = tree.new_node(NodeData::Text {
                contents: content.clone(),
            });
            tree.append_child(parent_id, text_id);
        }
        NodeKind::Comment { content } => {
            let comment_id = tree.new_node(NodeData::Comment {
                contents: content.clone(),
            });
            tree.append_child(parent_id, comment_id);
        }
        NodeKind::ProcessingInstruction { target, data } => {
            let pi_id = tree.new_node(NodeData::ProcessingInstruction {
                target: target.clone(),
                data: data.clone().unwrap_or_default(),
            });
            tree.append_child(parent_id, pi_id);
        }
        NodeKind::DocumentType { name, public_id, system_id, .. } => {
            let dt_id = tree.new_node(NodeData::Doctype {
                name: name.clone(),
                public_id: public_id.clone().unwrap_or_default(),
                system_id: system_id.clone().unwrap_or_default(),
            });
            tree.append_child(parent_id, dt_id);
        }
        // EntityRef, EntityDecl, Notation etc. — skip (rare in XHTML).
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_xml() {
        let tree = parse_xml("<?xml version=\"1.0\"?><root><child>text</child></root>");
        let root = tree.document();
        let children: Vec<_> = tree.children(root);
        assert!(!children.is_empty(), "document should have children");
    }

    #[test]
    fn parse_xhtml() {
        let tree = parse_xml(
            "<?xml version=\"1.0\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>Test</title></head><body><p>Hello</p></body></html>"
        );
        let root = tree.document();
        let children: Vec<_> = tree.children(root);
        assert!(!children.is_empty());
    }
}
