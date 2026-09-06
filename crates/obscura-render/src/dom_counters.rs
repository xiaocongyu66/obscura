//! DOM integration: build a taffy layout tree from a live [`DomTree`], run
//! layout, and return border-box geometry keyed by [`NodeId`].
//!
//! Phase 3. Text nodes do not yet contribute to size (no inline/text layout
//! until the text/paint phase), so a leaf element with only text may have zero
//! height. Block and flex structure, plus explicit sizes and box model, are
//! correct.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use obscura_dom::tree::{DomTree, NodeId};
use taffy::prelude::*;

use crate::{to_taffy_style, Rect};

use super::*;
use crate::dom::rendered_children;

#[derive(Default)]
pub(crate) struct CssCounterState {
    values: HashMap<String, Vec<i32>>,
}

impl CssCounterState {
    fn apply(
        &mut self,
        reset: &[crate::CounterDirective],
        increment: &[crate::CounterDirective],
        set: &[crate::CounterDirective],
    ) -> Vec<String> {
        let mut created = Vec::new();
        for directive in reset {
            self.values
                .entry(directive.name.clone())
                .or_default()
                .push(directive.value);
            created.push(directive.name.clone());
        }
        for directive in increment {
            let stack = self.values.entry(directive.name.clone()).or_default();
            if stack.is_empty() {
                stack.push(0);
                created.push(directive.name.clone());
            }
            if let Some(value) = stack.last_mut() {
                *value = value.saturating_add(directive.value);
            }
        }
        for directive in set {
            let stack = self.values.entry(directive.name.clone()).or_default();
            if stack.is_empty() {
                stack.push(0);
                created.push(directive.name.clone());
            }
            if let Some(value) = stack.last_mut() {
                *value = directive.value;
            }
        }
        created
    }

    fn pop_created(&mut self, created: &[String]) {
        for name in created.iter().rev() {
            if let Some(stack) = self.values.get_mut(name) {
                stack.pop();
                if stack.is_empty() {
                    self.values.remove(name);
                }
            }
        }
    }

    fn render(&self, items: &[crate::GeneratedContentItem]) -> String {
        let mut result = String::new();
        for item in items {
            match item {
                crate::GeneratedContentItem::Text(text) => result.push_str(text),
                crate::GeneratedContentItem::Counter { name, style } => {
                    let value = self
                        .values
                        .get(name)
                        .and_then(|stack| stack.last())
                        .copied()
                        .unwrap_or(0);
                    result.push_str(&crate::css::format_counter_value(value, *style));
                }
                crate::GeneratedContentItem::Counters {
                    name,
                    separator,
                    style,
                } => {
                    if let Some(stack) = self.values.get(name).filter(|stack| !stack.is_empty()) {
                        for (index, value) in stack.iter().enumerate() {
                            if index != 0 {
                                result.push_str(separator);
                            }
                            result.push_str(&crate::css::format_counter_value(*value, *style));
                        }
                    } else {
                        result.push_str(&crate::css::format_counter_value(0, *style));
                    }
                }
            }
        }
        result
    }
}

/// Resolve generated CSS counter text in tree order after the complete author
/// cascade is known. Counter scopes created by an element remain visible to
/// its descendants and following siblings, and expire with their shared
/// parent. That is the scope shape used by browser counter managers and covers
/// nested chapter numbering as well as line counters reset on a `<code>`.
pub(crate) fn resolve_css_counters(tree: &DomTree, styles: &mut HashMap<NodeId, crate::LayoutStyle>) {
    fn walk(
        tree: &DomTree,
        id: NodeId,
        styles: &mut HashMap<NodeId, crate::LayoutStyle>,
        counters: &mut CssCounterState,
    ) -> Vec<String> {
        let Some(node) = tree.get_node(id) else {
            return Vec::new();
        };
        if styles
            .get(&id)
            .is_some_and(|style| style.display == crate::Display::None)
        {
            return Vec::new();
        }

        let created = styles.get(&id).map_or_else(Vec::new, |style| {
            counters.apply(
                &style.counter_reset,
                &style.counter_increment,
                &style.counter_set,
            )
        });

        if let Some(style) = styles.get_mut(&id) {
            if let Some(pseudo) = style.before_pseudo.as_mut() {
                if let Some(items) = pseudo.generated_content.as_deref() {
                    pseudo.before_content = Some(counters.render(items));
                }
            }
            style.before_content = style
                .before_pseudo
                .as_ref()
                .filter(|pseudo| pseudo.position != Some(taffy::Position::Absolute))
                .and_then(|pseudo| pseudo.before_content.clone());
        }

        let mut child_scopes = Vec::new();
        for child in rendered_children(tree, id) {
            child_scopes.extend(walk(tree, child, styles, counters));
        }
        counters.pop_created(&child_scopes);

        if let Some(style) = styles.get_mut(&id) {
            if let Some(pseudo) = style.after_pseudo.as_mut() {
                if let Some(items) = pseudo.generated_content.as_deref() {
                    pseudo.before_content = Some(counters.render(items));
                }
            }
            style.after_content = style
                .after_pseudo
                .as_ref()
                .filter(|pseudo| pseudo.position != Some(taffy::Position::Absolute))
                .and_then(|pseudo| pseudo.before_content.clone());
        }

        // Non-element nodes cannot create counter scopes, but walking through
        // them keeps this robust to document fragments and template wrappers.
        let _ = node;
        created
    }

    let mut counters = CssCounterState::default();
    let root_scopes = walk(tree, tree.document(), styles, &mut counters);
    counters.pop_created(&root_scopes);
}

