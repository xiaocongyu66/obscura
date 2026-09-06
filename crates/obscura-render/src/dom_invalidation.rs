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

use crate::dom_counters::resolve_css_counters;
use crate::dom_sticky::{DerivedGeometryState, DerivedLayoutState, StickyFrame, StickyLayout};
use crate::{to_taffy_style, Rect};

use super::*;
use crate::dom::retained_attribute_mutation_kind;
use crate::dom::RetainedAttributeMutationKind;

pub(crate) enum RetainedStylePlan {
    Reuse {
        dirty: HashSet<NodeId>,
        has_animation_damage: bool,
    },
    Full,
}

fn add_style_subtree(tree: &DomTree, root: NodeId, dirty: &mut HashSet<NodeId>) {
    dirty.insert(root);
    dirty.extend(tree.descendants(root));
    // Rebuild the context chain too. A retained ancestor may carry a final
    // post-layout Px repair (native controls, blockification, table/grid
    // fixups) where a full pass would feed its specified Auto/inherit form to
    // the fresh descendant's normalization. Ancestors are fresh individually;
    // their unrelated sibling subtrees remain reusable.
    dirty.extend(tree.ancestors(root));
}

pub(crate) fn add_container_query_reset_scopes(
    tree: &DomTree,
    id: NodeId,
    sheet: &crate::css::Stylesheet,
    matcher: &mut obscura_dom::selector::Matcher,
    active_containers: &HashSet<NodeId>,
    inside_active_container: bool,
    selected_ancestor: bool,
    dirty: &mut HashSet<NodeId>,
) {
    let is_element = tree.get_node(id).is_some_and(|node| node.is_element());
    let can_query_here = inside_active_container || active_containers.contains(&id);
    let selected_here = !selected_ancestor
        && can_query_here
        && is_element
        && sheet.node_matches_container_query_rule(tree, matcher, id);
    let selected = selected_ancestor || selected_here;
    if selected {
        // The retained style is the previous converged, query-enabled value.
        // The convergence seed pass deliberately disables query rules, so
        // reset every possible subject and inherited descendant to its base
        // cascade before geometry is sampled again.
        dirty.insert(id);
    }
    if selected_here {
        // Stop as soon as an already-recorded context node is reached. An
        // earlier selected sibling necessarily inserted the rest of this same
        // ancestor chain, so each ancestor is visited at most once.
        let mut ancestor = tree.get_node(id).and_then(|node| node.parent);
        while let Some(parent) = ancestor {
            if !dirty.insert(parent) {
                break;
            }
            ancestor = tree.get_node(parent).and_then(|node| node.parent);
        }
    }
    if is_element {
        matcher.push_ancestor(tree, id);
    }
    let children_inside_container = inside_active_container || active_containers.contains(&id);
    for child in tree.children(id) {
        add_container_query_reset_scopes(
            tree,
            child,
            sheet,
            matcher,
            active_containers,
            children_inside_container,
            selected,
            dirty,
        );
    }
    if is_element {
        matcher.pop_ancestor();
    }
}

fn add_following_sibling_subtrees(
    tree: &DomTree,
    node: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let mut sibling = tree.get_node(node).and_then(|node| node.next_sibling);
    while let Some(id) = sibling {
        add_style_subtree(tree, id, dirty);
        sibling = tree.get_node(id).and_then(|node| node.next_sibling);
    }
}

fn subtree_contains_style_element(tree: &DomTree, root: NodeId) -> bool {
    std::iter::once(root)
        .chain(tree.descendants(root))
        .any(|id| {
            tree.get_node(id).is_some_and(|node| {
                node.as_element()
                    .is_some_and(|element| element.local.as_ref() == "style")
            })
        })
}

fn node_is_style_text(tree: &DomTree, node: NodeId, parent: Option<NodeId>) -> bool {
    parent.is_some_and(|parent| {
        tree.get_node(parent).is_some_and(|node| {
            node.as_element()
                .is_some_and(|element| element.local.as_ref() == "style")
        })
    }) || tree.get_node(node).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "style")
    })
}

fn subtree_may_match_relational_path(
    tree: &DomTree,
    invalidation: &crate::css::RelationalInvalidation,
    root: NodeId,
) -> bool {
    std::iter::once(root)
        .chain(tree.descendants(root))
        .any(|id| invalidation.relative_path_may_match(tree, id))
}

/// Candidate anchors after an insertion. Relative selectors search from the
/// changed subtree toward ancestors and earlier siblings; only the boundary
/// can connect the already-fresh inserted subtree to retained elements.
fn add_inserted_relational_anchor_candidates(
    tree: &DomTree,
    node: NodeId,
    candidates: &mut HashSet<NodeId>,
) {
    let mut current = Some(node);
    while let Some(id) = current {
        let mut sibling = tree.get_node(id).and_then(|node| node.prev_sibling);
        while let Some(previous) = sibling {
            candidates.insert(previous);
            sibling = tree.get_node(previous).and_then(|node| node.prev_sibling);
        }
        current = tree.get_node(id).and_then(|node| node.parent);
        if let Some(parent) = current {
            candidates.insert(parent);
        }
    }
}

/// Removal has already destroyed the old sibling links. As Gecko does for a
/// removal side effect, inspect every sibling at each old ancestor boundary;
/// this includes the old previous/next neighbors without retaining a DOM
/// snapshot in every mutation record.
fn add_removed_relational_anchor_candidates(
    tree: &DomTree,
    old_parent: NodeId,
    candidates: &mut HashSet<NodeId>,
) {
    let mut current = Some(old_parent);
    while let Some(id) = current {
        candidates.insert(id);
        candidates.extend(tree.children(id));
        let parent = tree.get_node(id).and_then(|node| node.parent);
        if let Some(parent) = parent {
            candidates.extend(tree.children(parent));
        }
        current = parent;
    }
}

fn add_relational_anchor_scope(
    tree: &DomTree,
    anchor: NodeId,
    reaches: crate::css::InvalidationReaches,
    dirty: &mut HashSet<NodeId>,
) {
    // Re-cascading the anchor subtree covers anchor-self changes, inheritance,
    // and every descendant subject. It is intentionally broader than the
    // dependency's exact reach but keeps the retained-style implementation
    // independent of selector matching internals.
    add_style_subtree(tree, anchor, dirty);
    if reaches.contains(crate::css::InvalidationReaches::SIBLINGS) {
        add_following_sibling_subtrees(tree, anchor, dirty);
    }
}

/// Apply Gecko-style upward `:has()` invalidation for one child-list or text
/// mutation. Returns false only when the path outside the anchor combines
/// traversals which the renderer's flat reach bits cannot represent soundly.
fn add_relational_tree_invalidation(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    mutation: &TreeStyleMutation,
    dirty: &mut HashSet<NodeId>,
) -> bool {
    if map.relational_invalidations().is_empty() {
        return true;
    }
    let mut candidates = HashSet::new();
    match *mutation {
        TreeStyleMutation::Insert {
            node,
            old_parent,
            ..
        } => {
            add_inserted_relational_anchor_candidates(tree, node, &mut candidates);
            if let Some(old_parent) = old_parent {
                add_removed_relational_anchor_candidates(tree, old_parent, &mut candidates);
            }
        }
        TreeStyleMutation::Remove { old_parent, .. } => {
            add_removed_relational_anchor_candidates(tree, old_parent, &mut candidates);
        }
        TreeStyleMutation::Text { node, parent } => {
            add_inserted_relational_anchor_candidates(tree, node, &mut candidates);
            if let Some(parent) = parent {
                candidates.insert(parent);
            }
        }
    }

    for invalidation in map.relational_invalidations() {
        let triggered = match mutation {
                TreeStyleMutation::Remove { .. } => true,
                TreeStyleMutation::Insert { node, .. } => {
                    subtree_may_match_relational_path(tree, invalidation, *node)
                        || invalidation.unkeyed_subject
                        || invalidation.sibling_side_effect
                        || invalidation.structural_side_effect
                }
                TreeStyleMutation::Text { .. } => invalidation.text_side_effect,
            };
        if !triggered {
            continue;
        }
        for anchor in candidates.iter().copied() {
            if !invalidation.anchor_may_match(tree, anchor) {
                continue;
            }
            if invalidation.unrepresentable_outer_path {
                if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                    eprintln!(
                        "[timing] retained-style fallback reason=relational-unrepresentable rule_order={} anchor={} mutation={mutation:?}",
                        invalidation.rule_order,
                        anchor.index(),
                    );
                }
                return false;
            }
            add_relational_anchor_scope(
                tree,
                anchor,
                invalidation.anchor_reaches,
                dirty,
            );
        }
    }
    true
}

fn add_style_context_chain(tree: &DomTree, node: NodeId, dirty: &mut HashSet<NodeId>) {
    dirty.insert(node);
    dirty.extend(tree.ancestors(node));
}

fn add_table_row_child_scope(tree: &DomTree, parent: NodeId, dirty: &mut HashSet<NodeId>) {
    let is_table_row = tree.get_node(parent).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "tr")
    });
    if !is_table_row {
        return;
    }
    // The table fallback assigns surplus growth to the trailing auto cell
    // after cascade. A child-list change can move that role even without an
    // authored structural selector, so reset every surviving cell from its
    // specified style before running the fixup again.
    for child in element_children(tree, parent) {
        add_style_subtree(tree, child, dirty);
    }
}

fn element_children(tree: &DomTree, parent: NodeId) -> Vec<NodeId> {
    tree.children(parent)
        .into_iter()
        .filter(|child| tree.get_node(*child).is_some_and(|node| node.is_element()))
        .collect()
}

fn element_local_name(tree: &DomTree, node: NodeId) -> Option<String> {
    tree.get_node(node)
        .and_then(|node| node.as_element().map(|element| element.local.to_string()))
}

/// Re-cascade a node whose structural pseudo state may have changed. Even a
/// self-only selector can change an inherited property, so the node's complete
/// subtree is the smallest renderer-independent safe unit. A structural state
/// used left of a sibling combinator additionally reaches following siblings.
fn add_structural_candidate_scope(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    state: &str,
    candidate: NodeId,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let invalidations = map
        .structural_invalidations(state)
        .into_iter()
        .filter(|invalidation| {
            !invalidation.inside_relational
                && invalidation.subject_may_match(tree, candidate)
        })
        .collect::<Vec<_>>();
    if invalidations.is_empty() {
        return;
    }
    if invalidations.iter().any(|invalidation| {
        invalidation
            .reaches
            .contains(crate::css::InvalidationReaches::CONSERVATIVE)
    }) {
        add_style_subtree(tree, parent, dirty);
        return;
    }
    add_style_subtree(tree, candidate, dirty);
    if invalidations.iter().any(|invalidation| {
        invalidation
            .reaches
            .contains(crate::css::InvalidationReaches::SIBLINGS)
    }) {
        add_following_sibling_subtrees(tree, candidate, dirty);
    }
}

fn add_inserted_structural_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    node: NodeId,
    parent: NodeId,
    mutations: &[RetainedStyleMutation],
    dirty: &mut HashSet<NodeId>,
) {
    let siblings = element_children(tree, parent);
    let Some(position) = siblings.iter().position(|candidate| *candidate == node) else {
        return;
    };
    let mut add = |state: &str, candidates: &[NodeId]| {
        for candidate in candidates.iter().copied() {
            add_structural_candidate_scope(tree, map, state, candidate, parent, dirty);
        }
    };

    let insertion_count = mutations
        .iter()
        .filter(|mutation| {
            matches!(
                mutation,
                RetainedStyleMutation::Tree(TreeStyleMutation::Insert {
                    node,
                    new_parent,
                    ..
                }) if *new_parent == parent
                    && tree.get_node(*node).is_some_and(|node| node.is_element())
            )
        })
        .count();
    if insertion_count > 1 {
        // Mutation records do not retain the old sibling boundaries. With two
        // fresh insertions, the old first/last/only child can sit beyond the
        // final two boundary nodes and would otherwise keep a stale match.
        // Direct children are still a bounded scope, and keyed structural
        // metadata avoids cascading unrelated candidates.
        for state in [
            "first-child",
            "last-child",
            "only-child",
            "first-of-type",
            "last-of-type",
            "only-of-type",
        ] {
            add(state, &siblings);
        }
    }

    if position == 0 {
        add("first-child", &siblings[..siblings.len().min(2)]);
    }
    if position + 1 == siblings.len() {
        add(
            "last-child",
            &siblings[position.saturating_sub(1)..],
        );
    }
    if siblings.len() <= 2 {
        add("only-child", &siblings);
    }
    add("nth-child", &siblings[position..]);
    add("nth-last-child", &siblings[..=position]);

    let Some(local) = element_local_name(tree, node) else {
        return;
    };
    let same_type = siblings
        .iter()
        .copied()
        .filter(|candidate| element_local_name(tree, *candidate).as_deref() == Some(&local))
        .collect::<Vec<_>>();
    let Some(type_position) = same_type.iter().position(|candidate| *candidate == node) else {
        return;
    };
    if type_position == 0 {
        add("first-of-type", &same_type[..same_type.len().min(2)]);
    }
    if type_position + 1 == same_type.len() {
        add(
            "last-of-type",
            &same_type[type_position.saturating_sub(1)..],
        );
    }
    if same_type.len() <= 2 {
        add("only-of-type", &same_type);
    }
    add("nth-of-type", &same_type[type_position..]);
    add("nth-last-of-type", &same_type[..=type_position]);
}

fn add_removed_structural_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    removed: NodeId,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let siblings = element_children(tree, parent);
    if let Some(first) = siblings.first().copied() {
        add_structural_candidate_scope(tree, map, "first-child", first, parent, dirty);
    }
    if let Some(last) = siblings.last().copied() {
        add_structural_candidate_scope(tree, map, "last-child", last, parent, dirty);
    }
    if siblings.len() <= 1 {
        for candidate in siblings.iter().copied() {
            add_structural_candidate_scope(tree, map, "only-child", candidate, parent, dirty);
        }
    }
    for state in ["nth-child", "nth-last-child"] {
        for candidate in siblings.iter().copied() {
            add_structural_candidate_scope(tree, map, state, candidate, parent, dirty);
        }
    }

    let Some(local) = element_local_name(tree, removed) else {
        return;
    };
    let same_type = siblings
        .iter()
        .copied()
        .filter(|candidate| element_local_name(tree, *candidate).as_deref() == Some(&local))
        .collect::<Vec<_>>();
    if let Some(first) = same_type.first().copied() {
        add_structural_candidate_scope(tree, map, "first-of-type", first, parent, dirty);
    }
    if let Some(last) = same_type.last().copied() {
        add_structural_candidate_scope(tree, map, "last-of-type", last, parent, dirty);
    }
    if same_type.len() <= 1 {
        for candidate in same_type.iter().copied() {
            add_structural_candidate_scope(tree, map, "only-of-type", candidate, parent, dirty);
        }
    }
    for state in ["nth-of-type", "nth-last-of-type"] {
        for candidate in same_type.iter().copied() {
            add_structural_candidate_scope(tree, map, state, candidate, parent, dirty);
        }
    }
}

fn add_inserted_sibling_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    node: NodeId,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let siblings = element_children(tree, parent);
    let Some(position) = siblings.iter().position(|candidate| *candidate == node) else {
        return;
    };
    let may_start_sibling_selector = map.node_may_start_sibling_selector(tree, node);
    if map.has_adjacent_sibling_selectors() {
        if position != 0 || may_start_sibling_selector {
            if let Some(next) = siblings.get(position + 1).copied() {
                add_style_subtree(tree, next, dirty);
            }
        }
    }
    if map.has_general_sibling_selectors() && may_start_sibling_selector {
        for following in siblings.iter().skip(position + 1).copied() {
            add_style_subtree(tree, following, dirty);
        }
    }
}

fn add_removed_sibling_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    if !map.has_adjacent_sibling_selectors() && !map.has_general_sibling_selectors() {
        return;
    }
    // Removal records intentionally do not retain old sibling pointers. All
    // direct element siblings are the bounded sound recovery set; their clean
    // descendant subtrees remain reusable when no sibling selector reaches
    // through them.
    for sibling in element_children(tree, parent) {
        add_style_subtree(tree, sibling, dirty);
    }
}

fn empty_state_may_have_changed(
    tree: &DomTree,
    parent: NodeId,
    mutations: &[RetainedStyleMutation],
) -> bool {
    let relevant_children = tree
        .children(parent)
        .into_iter()
        .filter(|child| {
            tree.get_node(*child).is_some_and(|node| match &node.data {
                obscura_dom::tree::NodeData::Element { .. } => true,
                obscura_dom::tree::NodeData::Text { contents } => !contents.is_empty(),
                _ => false,
            })
        })
        .count();
    let boundary_mutations = mutations
        .iter()
        .filter(|mutation| match mutation {
            RetainedStyleMutation::Attribute(_) => false,
            RetainedStyleMutation::Animation { .. } => false,
            RetainedStyleMutation::WaapiAnimation { .. } => false,
            RetainedStyleMutation::Resource => false,
            RetainedStyleMutation::Tree(TreeStyleMutation::Insert {
                old_parent,
                new_parent,
                ..
            }) => *new_parent == parent || *old_parent == Some(parent),
            RetainedStyleMutation::Tree(TreeStyleMutation::Remove { old_parent, .. }) => {
                *old_parent == parent
            }
            RetainedStyleMutation::Tree(TreeStyleMutation::Text {
                parent: text_parent,
                ..
            }) => *text_parent == Some(parent),
        })
        .count();
    // At least one relevant child untouched by this mutation batch proves the
    // parent was non-empty before and after every queued boundary operation.
    relevant_children <= boundary_mutations
}

fn add_empty_parent_scope(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    parent: NodeId,
    mutations: &[RetainedStyleMutation],
    dirty: &mut HashSet<NodeId>,
) {
    if !empty_state_may_have_changed(tree, parent, mutations) {
        return;
    }
    let invalidations = map
        .structural_invalidations("empty")
        .into_iter()
        .filter(|invalidation| {
            !invalidation.inside_relational
                && invalidation.subject_may_match(tree, parent)
        })
        .collect::<Vec<_>>();
    if invalidations.is_empty() {
        return;
    }
    add_style_subtree(tree, parent, dirty);
    if invalidations.iter().any(|invalidation| {
        invalidation
            .reaches
            .contains(crate::css::InvalidationReaches::SIBLINGS)
            || invalidation
                .reaches
                .contains(crate::css::InvalidationReaches::CONSERVATIVE)
    }) {
        add_following_sibling_subtrees(tree, parent, dirty);
    }
}

/// Convert old/new selector keys into a conservative set of fresh cascade
/// roots. Every selected root is expanded to its complete subtree so ordinary
/// CSS inheritance, custom properties, generated content, and later selector
/// compounds remain sound without retaining Gecko's full dependency chains.
pub(crate) fn retained_style_plan(
    tree: &DomTree,
    sheet: &crate::css::Stylesheet,
    mutations: &[RetainedStyleMutation],
) -> RetainedStylePlan {
    let mut dirty = HashSet::new();
    let mut has_animation_damage = false;
    for mutation in mutations {
        if matches!(mutation, RetainedStyleMutation::Resource) {
            continue;
        }
        if let RetainedStyleMutation::Animation { node } = mutation {
            // Animated color and visibility inherit, keyframe endpoints can
            // consume inherited custom properties, and generated pseudos are
            // rebuilt with their originating element. The existing subtree
            // expansion covers all three while retaining sibling branches.
            add_style_subtree(tree, *node, &mut dirty);
            has_animation_damage = true;
            continue;
        }
        if let RetainedStyleMutation::WaapiAnimation { node } = mutation {
            dirty.insert(*node);
            has_animation_damage = true;
            continue;
        }
        let RetainedStyleMutation::Attribute(mutation) = mutation else {
            let RetainedStyleMutation::Tree(mutation) = mutation else {
                unreachable!()
            };
            let map = sheet.invalidation_map();
            match *mutation {
                TreeStyleMutation::Insert {
                    node,
                    old_parent,
                    new_parent,
                } => {
                    // Inserting or moving a style subtree changes the ordered
                    // author stylesheet, so parsing/index reuse is forbidden.
                    if subtree_contains_style_element(tree, node)
                        || node_is_style_text(tree, node, old_parent)
                        || node_is_style_text(tree, node, Some(new_parent))
                    {
                        if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                            eprintln!(
                                "[timing] retained-style fallback reason=style-subtree-insert node={} old_parent={old_parent:?} new_parent={}",
                                node.index(),
                                new_parent.index(),
                            );
                        }
                        return RetainedStylePlan::Full;
                    }
                    if !add_relational_tree_invalidation(tree, map, mutation, &mut dirty) {
                        return RetainedStylePlan::Full;
                    }
                    add_style_subtree(tree, node, &mut dirty);
                    if let Some(parent) = old_parent {
                        add_style_context_chain(tree, parent, &mut dirty);
                        add_table_row_child_scope(tree, parent, &mut dirty);
                        add_removed_structural_scopes(tree, map, node, parent, &mut dirty);
                        add_removed_sibling_scopes(tree, map, parent, &mut dirty);
                        add_empty_parent_scope(tree, map, parent, mutations, &mut dirty);
                    }
                    add_style_context_chain(tree, new_parent, &mut dirty);
                    add_table_row_child_scope(tree, new_parent, &mut dirty);
                    add_inserted_structural_scopes(
                        tree,
                        map,
                        node,
                        new_parent,
                        mutations,
                        &mut dirty,
                    );
                    add_inserted_sibling_scopes(tree, map, node, new_parent, &mut dirty);
                    add_empty_parent_scope(tree, map, new_parent, mutations, &mut dirty);
                }
                TreeStyleMutation::Remove { node, old_parent } => {
                    if subtree_contains_style_element(tree, node)
                        || node_is_style_text(tree, node, Some(old_parent))
                    {
                        if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                            eprintln!(
                                "[timing] retained-style fallback reason=style-subtree-remove node={} old_parent={}",
                                node.index(),
                                old_parent.index(),
                            );
                        }
                        return RetainedStylePlan::Full;
                    }
                    if !add_relational_tree_invalidation(tree, map, mutation, &mut dirty) {
                        return RetainedStylePlan::Full;
                    }
                    add_style_context_chain(tree, old_parent, &mut dirty);
                    add_table_row_child_scope(tree, old_parent, &mut dirty);
                    add_removed_structural_scopes(tree, map, node, old_parent, &mut dirty);
                    add_removed_sibling_scopes(tree, map, old_parent, &mut dirty);
                    add_empty_parent_scope(tree, map, old_parent, mutations, &mut dirty);
                }
                TreeStyleMutation::Text { node, parent } => {
                    if node_is_style_text(tree, node, parent) {
                        if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                            eprintln!(
                                "[timing] retained-style fallback reason=style-text node={} parent={parent:?}",
                                node.index(),
                            );
                        }
                        return RetainedStylePlan::Full;
                    }
                    if !add_relational_tree_invalidation(tree, map, mutation, &mut dirty) {
                        return RetainedStylePlan::Full;
                    }
                    if let Some(parent) = parent {
                        add_style_context_chain(tree, parent, &mut dirty);
                        add_empty_parent_scope(tree, map, parent, mutations, &mut dirty);
                    } else {
                        dirty.insert(node);
                    }
                }
            }
            continue;
        };
        let name = mutation.name.to_ascii_lowercase();
        match retained_attribute_mutation_kind(tree, mutation.node, &name) {
            RetainedAttributeMutationKind::Full => return RetainedStylePlan::Full,
            RetainedAttributeMutationKind::Subtree => {
                // Mapped hints and native-control sizing enter the mutable
                // LayoutStyle graph. Re-cascade from specified values before
                // post-cascade used-value repair runs again.
                add_style_subtree(tree, mutation.node, &mut dirty);
            }
            RetainedAttributeMutationKind::Selector => {}
        }

        let map = sheet.invalidation_map();
        let mut dependencies = Vec::new();
        dependencies.extend_from_slice(map.attribute_dependencies(&name));
        if name == "id" {
            if let Some(old) = mutation.old_value.as_deref() {
                dependencies.extend_from_slice(map.id_dependencies(old));
            }
            if let Some(new) = mutation.new_value.as_deref() {
                dependencies.extend_from_slice(map.id_dependencies(new));
            }
        } else if name == "class" {
            for class in mutation
                .old_value
                .iter()
                .chain(&mutation.new_value)
                .flat_map(|value| value.split_whitespace())
            {
                dependencies.extend_from_slice(map.class_dependencies(class));
            }
        }

        // Several HTML boolean/value attributes also back selector pseudo
        // states. Include both sides of paired states so removing an attribute
        // invalidates rules such as `:enabled` and `:optional`, not only the
        // positive state.
        let states: &[&str] = match name.as_str() {
            "checked" | "selected" => &["checked"],
            "disabled" => &["disabled", "enabled"],
            "dir" => &["dir"],
            "href" => &["any-link", "link", "visited"],
            "lang" | "xml:lang" => &["lang"],
            "open" => &["open"],
            "placeholder" | "value" => &["placeholder-shown"],
            "readonly" => &["read-only", "read-write"],
            "required" => &["required", "optional"],
            _ => &[],
        };
        for state in states {
            dependencies.extend_from_slice(map.state_dependencies(state));
        }

        for dependency in dependencies {
            let reaches = dependency.reaches;
            if reaches.contains(crate::css::InvalidationReaches::CONSERVATIVE) {
                return RetainedStylePlan::Full;
            }
            if reaches.contains(crate::css::InvalidationReaches::SELF)
                || reaches.contains(crate::css::InvalidationReaches::DESCENDANTS)
            {
                add_style_subtree(tree, mutation.node, &mut dirty);
            }
            if reaches.contains(crate::css::InvalidationReaches::SIBLINGS) {
                add_following_sibling_subtrees(tree, mutation.node, &mut dirty);
            }
        }
    }
    RetainedStylePlan::Reuse {
        dirty,
        has_animation_damage,
    }
}

