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
use crate::dom_invalidation::{add_container_query_reset_scopes, retained_style_plan, RetainedStylePlan};
use crate::dom_tables::{apply_fit_content_widths, auto_table_percentage_intrinsic_floor, build_table, distribute_auto_table_columns, distribute_fixed_table_columns, reliable_declared_content_width, reliable_normal_flow_content_width, reliable_ratio_only_available_width, reliable_table_available_width, synthesize_row_rects, table_ancestor_depth, table_inline_outer_edges, table_spacing};
use crate::dom_entry::{ContainerLayoutTermination, ContainerLayoutTelemetry, CONTAINER_LAYOUT_SAFETY_LIMIT, container_iteration_termination};
pub(crate) use crate::dom_entry::{ReplacedIntrinsicMap,
    layout_dom, layout_dom_with_images, layout_dom_with_resources,
    layout_dom_with_web_fonts, layout_dom_with_web_fonts_and_stylesheet_cache,
    layout_dom_with_web_fonts_and_stylesheet_cache_at_animation_time,
    layout_dom_with_web_fonts_and_stylesheet_cache_with_animation_state,
    layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state,
    layout_dom_with_web_fonts_and_retained_styles,
    layout_dom_with_web_fonts_and_retained_styles_at_animation_time,
    layout_dom_with_web_fonts_and_retained_styles_with_animation_state,
    layout_dom_with_web_fonts_measured, layout_dom_with_web_fonts_pass_limit,
    layout_dom_with_web_fonts_pass_limit_at_animation_time,
    collect_shadow_stylesheets};
use crate::dom_grid::{apply_float_continuations, apply_full_span_column_subgrids, collect_effective_grid_children, compute_absolute_rects, effective_grid_child_style, reparent_inset_positioned_nodes, resolve_grid_areas, taffy_global_origin, EffectiveGridChild, StaticPositionCandidate};
use crate::dom_sticky::{DerivedGeometryState, DerivedLayoutState, StickyFrame, StickyLayout};
use crate::{to_taffy_style, Rect};

use super::*;
use crate::dom::{FloatContinuation, build, build_any, build_mixed_block, build_in_flow_pseudo, has_in_flow_generated_pseudo, effective_container_type, style_children, blockify_layout_children, blockify_generated_pseudos, tokenize_with_spaces, build_text_words, build_shaped_word_leaves, build_word_leaves, build_pseudo_content, pseudo_requires_generated_box, folded_inline_relative_offset, synthesize_ordinary_inline_fragments, synthesize_shaped_inline_fragments, shaped_item_clip, has_inline_content, GeneratedBoxKind, IfcRegistry, rendered_children, rendered_parent};

/// Approximate `float: left|right` without real per-line reflow (which
/// taffy's block/flex/grid modes do not provide): place the float alongside
/// the flow siblings that follow it until their estimated height reaches the
/// float's estimated bottom, a matching `clear` is encountered, or another
/// float begins, then let everything from there on revert to normal full-width
/// flow.
///
/// This is not a general CSS float implementation (a float taller than its
/// estimated flow zone won't reflow correctly), but it directly targets the overwhelmingly
/// common real-world shape: a floated image or infobox near the top of an
/// article, sitting beside the intro text, with the rest of the content
/// running full width once normal flow passes the float.
/// Rough height budget for a float with no explicit size and no images
/// (an icon-only or empty float, rare in practice): enough for a couple of
/// lines of caption-sized text without being so generous it drags in a
/// whole section the way an unbounded zone did.
const DEFAULT_FLOAT_HEIGHT_ESTIMATE: f32 = 200.0;

/// Estimate a float's rendered height in CSS px, for bounding how many flow
/// siblings should wrap alongside it (see `build_children_with_float_zone`).
/// Real layout hasn't run yet at this point (we're still building the taffy
/// tree), so this can only approximate: prefer an explicit height on the
/// float itself, else sum the explicit heights of descendant `<img>`s (the
/// common `<figure><img height=".."><figcaption>` thumbnail shape) plus a
/// text-based estimate of the float's own content (the common tall-infobox
/// shape, where the height comes from many rows of text rather than a
/// single image).
fn estimate_float_height(
    tree: &DomTree,
    float_id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> f32 {
    if let Some(crate::Dimension::Px(h)) = styles.get(&float_id).map(|s| s.height) {
        return h;
    }
    let image_height: f32 = tree
        .descendants(float_id)
        .into_iter()
        .filter(|&id| {
            tree.get_node(id)
                .and_then(|n| n.as_element().map(|e| e.local.to_string()))
                .as_deref()
                == Some("img")
        })
        .filter_map(|id| match styles.get(&id).map(|s| s.height) {
            Some(crate::Dimension::Px(h)) => Some(h),
            _ => None,
        })
        .sum();
    const ASSUMED_FLOAT_WIDTH: f32 = 280.0;
    let text_height = estimate_text_height(tree, float_id, styles, ASSUMED_FLOAT_WIDTH);
    // Flattened character count misses forced rows. A sidebar list with twenty
    // short `<li>`s or an infobox table with many `<tr>`s is much taller than
    // the same text treated as one wrapping paragraph. Add one line for each
    // structural row; the continuous-text estimate still accounts for extra
    // wrapping within those rows.
    let structural_height: f32 = tree
        .descendants(float_id)
        .into_iter()
        .filter(|&id| {
            tree.get_node(id)
                .and_then(|node| node.as_element().map(|element| element.local.to_string()))
                .map(|local| {
                    matches!(
                        local.as_str(),
                        "li" | "tr"
                            | "dt"
                            | "dd"
                            | "p"
                            | "figcaption"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                    )
                })
                .unwrap_or(false)
        })
        .map(|id| {
            styles
                .get(&id)
                .and_then(|style| style.font_size)
                .unwrap_or(16.0)
                * 1.2
        })
        .sum();
    (image_height + text_height + structural_height).max(DEFAULT_FLOAT_HEIGHT_ESTIMATE)
}

/// Estimate how tall `id`'s text content would render at `assumed_width`,
/// using the same average-character-width heuristic as the layout-only
/// (non-`paint`) text sizing fallback. Used only to bound the float-wrapping
/// zone (see `build_children_with_float_zone`), where the real available
/// width is not yet known, so this is deliberately approximate.
fn estimate_text_height(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    assumed_width: f32,
) -> f32 {
    let char_count = tree
        .text_content(id)
        .chars()
        .filter(|c| !c.is_whitespace())
        .count() as f32;
    if char_count == 0.0 {
        return 0.0;
    }
    let fsize = styles.get(&id).and_then(|s| s.font_size).unwrap_or(16.0);
    const AVG_CHAR_WIDTH_EM: f32 = 0.55;
    let chars_per_line = (assumed_width / (fsize * AVG_CHAR_WIDTH_EM)).max(1.0);
    let lines = (char_count / chars_per_line).ceil().max(1.0);
    lines * fsize * 1.2 + 16.0
}

/// Estimate the normal-flow height consumed by one sibling alongside a float.
/// Text alone is insufficient for image grids and fixed-height boxes, which
/// otherwise cost zero budget and remain squeezed beside a float long after
/// they should have passed its bottom.
fn estimate_flow_sibling_height(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    assumed_width: f32,
) -> f32 {
    let style = styles.get(&id);
    if style
        .map(|style| style.display == crate::Display::None)
        .unwrap_or(false)
        || style
            .and_then(|style| style.position)
            .map(|position| position == taffy::Position::Absolute)
            .unwrap_or(false)
    {
        return 0.0;
    }
    let explicit_height = match style.map(|style| style.height) {
        Some(crate::Dimension::Px(height)) => height.max(0.0),
        _ => 0.0,
    };
    let descendant_image_height: f32 = tree
        .descendants(id)
        .into_iter()
        .filter(|&descendant| {
            tree.get_node(descendant)
                .and_then(|node| {
                    node.as_element()
                        .map(|element| element.local.as_ref() == "img")
                })
                .unwrap_or(false)
        })
        .filter_map(
            |descendant| match styles.get(&descendant).map(|style| style.height) {
                Some(crate::Dimension::Px(height)) => Some(height.max(0.0)),
                _ => None,
            },
        )
        .sum();
    let own_image_height = if tree
        .get_node(id)
        .and_then(|node| {
            node.as_element()
                .map(|element| element.local.as_ref() == "img")
        })
        .unwrap_or(false)
    {
        explicit_height
    } else {
        0.0
    };
    let content_height = estimate_text_height(tree, id, styles, assumed_width)
        .max(explicit_height)
        .max(descendant_image_height + own_image_height);
    let margins = style
        .map(|style| (style.margin.top + style.margin.bottom).max(0.0))
        .unwrap_or(0.0);
    content_height + margins
}

/// Largest definite (px) width among `id` and its descendants. Used to cap an
/// auto-width floated figure: a Wikipedia thumbnail is `<figure ...><img
/// width=250><figcaption>long text</figcaption></figure>`, and without a cap
/// the caption's unwrapped one-line max-content width (~700px) sizes the float
/// and starves the adjacent flow column to nothing. Real browsers size the
/// figure to the image (display:table) and wrap the caption; the image's
/// definite width is the bound that reproduces that.
fn max_definite_descendant_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<f32> {
    let mut best: Option<f32> = None;
    for d in tree.descendants(id) {
        if let Some(crate::Dimension::Px(w)) = styles.get(&d).map(|s| s.width.clone()) {
            if w > 0.0 {
                best = Some(best.map_or(w, |b: f32| b.max(w)));
            }
        }
    }
    best
}

/// Fixed content descendants can floor a table's intrinsic minimum when the
/// generic grid measurement fails to surface them through an intervening
/// formatting context. Width hints on cells/rows/columns are different: the
/// CSS table algorithm treats those as preferred column constraints, so they
/// must remain shrinkable down to the content minimum.
pub(crate) fn max_definite_table_content_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<f32> {
    let mut best: Option<f32> = None;
    for descendant in tree.descendants(id) {
        let structural = styles
            .get(&descendant)
            .is_some_and(|style| style.is_table_cell_box)
            || tree.get_node(descendant).is_some_and(|node| {
                node.as_element().is_some_and(|element| {
                    matches!(
                        element.local.as_ref(),
                        "caption"
                            | "col"
                            | "colgroup"
                            | "thead"
                            | "tbody"
                            | "tfoot"
                            | "tr"
                            | "td"
                            | "th"
                    )
                })
            });
        if structural {
            continue;
        }
        if let Some(crate::Dimension::Px(width)) = styles.get(&descendant).map(|style| style.width)
        {
            if width > 0.0 {
                best = Some(best.map_or(width, |current| current.max(width)));
            }
        }
    }
    best
}

pub(crate) fn establishes_block_formatting_context(style: &crate::LayoutStyle) -> bool {
    matches!(style.display, crate::Display::Flex | crate::Display::Grid)
        || effective_container_type(style) != crate::ContainerType::Normal
        || style.flow_root
        || (style.overflow_scroll_container && !style.overflow_propagated_to_viewport)
        || style.is_inline_block
        || style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
}

fn clear_matches_float_sides(clear: crate::Clear, has_left: bool, has_right: bool) -> bool {
    (!has_left || matches!(clear, crate::Clear::Left | crate::Clear::Both))
        && (!has_right || matches!(clear, crate::Clear::Right | crate::Clear::Both))
}

fn has_deferred_or_auto_margin(style: &crate::LayoutStyle) -> bool {
    style.margin_auto.iter().any(|value| *value)
        || style.margin_percent.iter().any(Option::is_some)
        || style.margin_relative.iter().any(Option::is_some)
        || style.margin_expressions.iter().any(Option::is_some)
}

fn is_structural_native_clear_box(tree: &DomTree, id: NodeId, style: &crate::LayoutStyle) -> bool {
    style.display == crate::Display::Block
        && !style.display_contents
        && !style.is_table_box
        && !style.is_inline_block
        && !style.flow_root
        && !style.overflow_hidden
        && effective_container_type(style) == crate::ContainerType::Normal
        && style.float.is_none()
        && style.clear.is_some()
        && !matches!(style.position, Some(taffy::Position::Absolute))
        && tree.text_content(id).trim().is_empty()
        && style.before_pseudo.is_none()
        && style.after_pseudo.is_none()
}

fn is_structural_native_float_pseudo(style: &crate::LayoutStyle) -> bool {
    style.display != crate::Display::None
        && style.display != crate::Display::Inline
        && !style.display_contents
        && !style.is_inline_block
        && (!style.flow_root || style.is_table_box)
        && !style.overflow_hidden
        && effective_container_type(style) == crate::ContainerType::Normal
        && style.float.is_none()
        && !matches!(style.position, Some(taffy::Position::Absolute))
        && style
            .before_content
            .as_deref()
            .map_or(true, |content| content.trim().is_empty())
}

/// Native taffy floats are currently sound for a deliberately small structural
/// subset: a flat block band made from boxed direct floats, boxed block flow,
/// and empty generated/direct clearance boxes. Text nodes directly in the
/// parent, transparent wrappers, independent formatting contexts, and tables
/// retain the synthetic float-zone path.
pub(crate) fn can_use_native_float_band(
    tree: &DomTree,
    parent_style: &crate::LayoutStyle,
    dom_children: &[NodeId],
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    if parent_style.display != crate::Display::Block
        || parent_style.internal_flex_container
        || parent_style.is_inline_block
        || parent_style.is_table_box
        || matches!(parent_style.position, Some(taffy::Position::Absolute))
        || !matches!(
            parent_style.width,
            crate::Dimension::Auto | crate::Dimension::Px(_) | crate::Dimension::Percent(_)
        )
        || parent_style.size_expressions[0].is_some()
    {
        return false;
    }

    let mut has_left = false;
    let mut has_right = false;
    let mut saw_float = false;
    let mut saw_clear_after_floats = false;
    let mut saw_flow_after_floats = false;

    if let Some(before) = parent_style.before_pseudo.as_deref() {
        if !is_structural_native_float_pseudo(before) {
            return false;
        }
    }

    for &id in dom_children {
        let Some(node) = tree.get_node(id) else {
            continue;
        };
        if !node.is_element() {
            // Comments, doctypes, and processing instructions generate no
            // formatting box and cannot split a float band. In particular,
            // Bootstrap labels closing navbar wrappers with non-empty HTML
            // comments between the floated header and ordinary collapse
            // block. Only actual non-whitespace text requires the legacy
            // inline float path.
            if matches!(
                &node.data,
                obscura_dom::tree::NodeData::Text { contents }
                    if !contents.trim().is_empty()
            ) {
                return false;
            }
            continue;
        }
        let Some(style) = styles.get(&id) else {
            return false;
        };
        if style.display == crate::Display::None {
            continue;
        }

        if let Some(side) = style.float {
            if saw_clear_after_floats
                || saw_flow_after_floats
                || style.display_contents
                || style.is_table_box
                || matches!(style.position, Some(taffy::Position::Absolute))
                || !matches!(
                    style.width,
                    crate::Dimension::Auto | crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                )
                || style.size_expressions[0].is_some()
                || has_deferred_or_auto_margin(style)
            {
                return false;
            }
            saw_float = true;
            has_left |= side == crate::Float::Left;
            has_right |= side == crate::Float::Right;
        } else if is_structural_native_clear_box(tree, id, style) {
            if !saw_float {
                return false;
            }
            saw_clear_after_floats |= clear_matches_float_sides(
                style.clear.expect("structural clear has a side"),
                has_left,
                has_right,
            );
        } else if saw_float
            && style.display == crate::Display::Block
            && !style.display_contents
            && !style.is_inline_block
            && !style.is_table_box
            && !style.internal_flex_container
            && !matches!(style.position, Some(taffy::Position::Absolute))
        {
            // Gecko keeps an ordinary block's border box at the BFC's full
            // inline size and narrows only its descendant line boxes. A block
            // that establishes its own BFC (overflow/flow-root) instead moves
            // as one float-avoiding box. Taffy's native block float context
            // implements both branches; the old synthetic flex row could
            // represent neither distinction because it shrank every sibling's
            // outer box beside the float.
            saw_flow_after_floats = true;
        } else {
            return false;
        }
    }

    if let Some(after) = parent_style.after_pseudo.as_deref() {
        if !is_structural_native_float_pseudo(after) {
            return false;
        }
        if let Some(clear) = after.clear {
            saw_clear_after_floats |= clear_matches_float_sides(clear, has_left, has_right);
        }
    }

    // Taffy represents scroll-container overflow as a real BFC root. Plain
    // `clip` does not establish a BFC, and viewport-propagated overflow leaves
    // its source box visible. Other Obscura BFC markers do not yet have a
    // distinct taffy-side representation, so they are not an escape signal.
    let parent_is_native_bfc =
        parent_style.overflow_scroll_container && !parent_style.overflow_propagated_to_viewport;
    saw_float && (saw_flow_after_floats || saw_clear_after_floats || parent_is_native_bfc)
}

fn set_native_float_clear(
    taffy_tree: &mut TaffyTree<usize>,
    node: taffy::NodeId,
    style: &crate::LayoutStyle,
    generated_pseudo: bool,
) {
    let Ok(current) = taffy_tree.style(node) else {
        return;
    };
    let mut native = current.clone();
    native.float = match style.float {
        Some(crate::Float::Left) => taffy::style::Float::Left,
        Some(crate::Float::Right) => taffy::style::Float::Right,
        None => taffy::style::Float::None,
    };
    native.clear = match style.clear {
        Some(crate::Clear::Left) => taffy::style::Clear::Left,
        Some(crate::Clear::Right) => taffy::style::Clear::Right,
        Some(crate::Clear::Both) => taffy::style::Clear::Both,
        None => taffy::style::Clear::None,
    };
    if generated_pseudo && style.is_table_box {
        // Bootstrap-style clearfix pseudos use display:table only to generate
        // an empty block formatting box. Taffy's independent table-item clear
        // path is incomplete, while its same-BFC block clear path is correct.
        native.display = taffy::style::Display::Block;
        native.item_is_table = false;
    }
    let _ = taffy_tree.set_style(node, native);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_children_with_native_float_band(
    tree: &DomTree,
    parent_id: NodeId,
    parent_style: &crate::LayoutStyle,
    dom_children: &[NodeId],
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<taffy::NodeId> {
    let mut result = Vec::new();
    for (kind, pseudo) in [
        (
            GeneratedBoxKind::Before,
            parent_style.before_pseudo.as_deref(),
        ),
        (
            GeneratedBoxKind::After,
            parent_style.after_pseudo.as_deref(),
        ),
    ] {
        if kind == GeneratedBoxKind::After {
            for &id in dom_children {
                let Some(style) = styles.get(&id) else {
                    continue;
                };
                if style.float.is_none()
                    && style.display == crate::Display::Block
                    && !establishes_block_formatting_context(style)
                {
                    ifc_items.float_aware_blocks.insert(id);
                }
                for node in build_any(
                    tree, id, taffy_tree, id_map, words, engine, ifc_items, styles,
                ) {
                    set_native_float_clear(taffy_tree, node, style, false);
                    result.push(node);
                }
            }
        }
        if let Some((nodes, _)) = build_in_flow_pseudo(
            parent_id, kind, pseudo, taffy_tree, words, engine, ifc_items,
        ) {
            if let Some(style) = pseudo {
                for node in nodes {
                    set_native_float_clear(taffy_tree, node, style, true);
                    result.push(node);
                }
            }
        }
    }
    result
}

pub(crate) fn build_children_with_float_zone(
    tree: &DomTree,
    parent_id: NodeId,
    dom_children: &[NodeId],
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<taffy::NodeId> {
    let is_float = |cid: NodeId| {
        styles
            .get(&cid)
            .map(|style| style.display != crate::Display::None && style.float.is_some())
            .unwrap_or(false)
    };

    // A definite-height block whose only substantive contents are either one
    // full-width float or one percentage float followed by one percentage
    // inline atom has a bounded, single float band.  The general legacy path
    // below cannot represent that shape: it puts the float in an auto-width,
    // zero-height escape wrapper, so both percentage axes resolve against an
    // indefinite synthetic box.  A Bootstrap `width:100%;height:100%` column
    // then collapses or moves away from block-start, and the equally common
    // `[float:left;width:50%][inline-block;width:50%]` control row gives the
    // float zero size.
    //
    // Keep this deliberately narrower than a float manager.  In this exact
    // one-band case a full-size flex row is only a placement representation:
    // its percentage basis is the real containing block's content box, its
    // block-start is the normal flow position, and it cannot incorrectly
    // contain an escaping float because the parent already owns a definite
    // block size.  Text, multiple flow siblings, auto sizes, and floats that
    // can extend beyond an auto-height parent retain the general path.
    let parent_has_definite_height = styles.get(&parent_id).is_some_and(|parent| {
        matches!(
            parent.height,
            crate::Dimension::Px(_) | crate::Dimension::Percent(_)
        ) && parent.size_expressions[1].is_none()
    });
    let parent_has_clearfix = styles.get(&parent_id).is_some_and(|parent| {
        parent.after_pseudo.as_deref().is_some_and(|after| {
            matches!(after.clear, Some(crate::Clear::Left | crate::Clear::Both))
                && has_in_flow_generated_pseudo(Some(after))
        })
    });
    let parent_min_height = styles
        .get(&parent_id)
        .and_then(|parent| match parent.min_height {
            crate::Dimension::Px(value) => Some(value),
            _ => None,
        });
    if parent_has_definite_height || parent_has_clearfix || parent_min_height.is_some() {
        let fills_axis = |value: f32| (value - 1.0).abs() < 0.001;
        let substantive: Vec<NodeId> = dom_children
            .iter()
            .copied()
            .filter(|cid| {
                tree.get_node(*cid).is_some_and(|node| {
                    if node.is_element() {
                        styles
                            .get(cid)
                            .is_some_and(|style| style.display != crate::Display::None)
                    } else {
                        !tree.text_content(*cid).trim().is_empty()
                    }
                })
            })
            .collect();
        let floated: Vec<NodeId> = substantive
            .iter()
            .copied()
            .filter(|cid| is_float(*cid))
            .collect();
        if floated.len() == 1 {
            let float_dom = floated[0];
            let float_style = styles.get(&float_dom);
            let float_percent_width = float_style.and_then(|style| match style.width {
                crate::Dimension::Percent(value) => Some(value),
                _ => None,
            });
            let float_fills_height = float_style.is_some_and(|style| {
                matches!(style.height, crate::Dimension::Percent(value) if fills_axis(value))
                    && style.size_expressions[1].is_none()
            });
            let float_pixel_height = float_style.and_then(|style| match style.height {
                crate::Dimension::Px(value) if style.size_expressions[1].is_none() => Some(value),
                _ => None,
            });
            let float_has_definite_height = float_pixel_height.is_some() || float_fills_height;
            let height_is_already_contained = parent_has_definite_height
                || parent_has_clearfix
                || parent_min_height
                    .zip(float_pixel_height)
                    .is_some_and(|(minimum, height)| minimum + 0.001 >= height);
            let sole_full_width_float = substantive.len() == 1
                && float_percent_width.is_some_and(fills_axis)
                && float_has_definite_height
                && height_is_already_contained;
            let split_band_flow = if parent_has_definite_height
                && substantive.len() == 2
                && substantive[0] == float_dom
                && float_style.and_then(|style| style.float) == Some(crate::Float::Left)
                && float_fills_height
            {
                let flow_dom = substantive[1];
                styles.get(&flow_dom).and_then(|flow_style| {
                    let flow_percent_width = match flow_style.width {
                        crate::Dimension::Percent(value) => value,
                        _ => return None,
                    };
                    let flow_fills_height = matches!(
                        flow_style.height,
                        crate::Dimension::Percent(value) if fills_axis(value)
                    ) && flow_style.size_expressions[1].is_none();
                    let widths_fill_one_band = float_percent_width.is_some_and(|float_width| {
                        float_width > 0.0
                            && flow_percent_width > 0.0
                            && (float_width + flow_percent_width - 1.0).abs() < 0.001
                    });
                    (flow_style.display != crate::Display::None
                        && !flow_style.display_contents
                        && flow_style.is_inline_block
                        && flow_fills_height
                        && widths_fill_one_band)
                        .then_some(flow_dom)
                })
            } else {
                None
            };

            if sole_full_width_float || split_band_flow.is_some() {
                let float_node = build(
                    tree, float_dom, taffy_tree, id_map, words, engine, ifc_items, styles,
                );
                // The split-band guard admits only a boxed inline-block
                // element, so it must be built as one atomic box. Calling
                // `build_any` here would be needlessly fragile: its text and
                // transparent-inline branches may fan out into several nodes,
                // and discovering that only after construction would leave
                // detached nodes behind when falling back to the general path.
                let flow_node = split_band_flow.and_then(|flow_dom| {
                    build(
                        tree, flow_dom, taffy_tree, id_map, words, engine, ifc_items, styles,
                    )
                });
                if let Some(float_node) = float_node {
                    if sole_full_width_float || flow_node.is_some() {
                        let children: Vec<taffy::NodeId> = [Some(float_node), flow_node]
                            .into_iter()
                            .flatten()
                            .collect();
                        let band_style = taffy::Style {
                            display: taffy::style::Display::Flex,
                            flex_direction: taffy::FlexDirection::Row,
                            flex_wrap: taffy::FlexWrap::NoWrap,
                            align_items: Some(taffy::AlignItems::FLEX_START),
                            size: taffy::Size {
                                width: taffy::Dimension::percent(1.0),
                                height: if parent_has_definite_height {
                                    taffy::Dimension::percent(1.0)
                                } else {
                                    taffy::Dimension::auto()
                                },
                            },
                            ..Default::default()
                        };
                        if let Ok(band) = taffy_tree.new_with_children(band_style, &children) {
                            return vec![band];
                        }
                    }
                }
            }
        }
    }

    // A block made entirely from inline-ish flow content and two or more
    // right floats is the classic utility/navigation bar. Right floats are
    // placed from the inline end inward, so their visual order is the reverse
    // of source order, while ordinary inline content keeps filling from the
    // start of the same band. Serializing each encountered float into its own
    // synthetic row reverses those two groups and can leave the entire bar
    // shrink-wrapped at the start.
    //
    // Model this bounded one-band case as [flow | reversed right-float group].
    // A nested group preserves every float's authored margins; only the
    // anonymous group receives the auto margin used to represent the free
    // space between the two sides.
    let right_floats: Vec<NodeId> = dom_children
        .iter()
        .copied()
        .filter(|cid| {
            styles.get(cid).map_or(false, |style| {
                style.float == Some(crate::Float::Right) && style.display != crate::Display::None
            })
        })
        .collect();
    let has_left_float = dom_children.iter().any(|cid| {
        styles.get(cid).map_or(false, |style| {
            style.float == Some(crate::Float::Left) && style.display != crate::Display::None
        })
    });
    let flow_is_inline = dom_children.iter().all(|cid| {
        let Some(node) = tree.get_node(*cid) else {
            return true;
        };
        if !node.is_element() || is_float(*cid) {
            return true;
        }
        styles
            .get(cid)
            .map_or(true, |style| style.display != crate::Display::Block)
    });
    if right_floats.len() >= 2 && !has_left_float && flow_is_inline {
        // Removing out-of-flow items must not remove the one collapsible
        // space between the inline items on either side of them. Collapse
        // any run of formatting whitespace to one representative node, while
        // dropping leading/trailing whitespace at the band edges.
        let mut flow_dom = Vec::new();
        let mut pending_whitespace = None;
        let mut has_flow_content = false;
        for &cid in dom_children {
            if is_float(cid) {
                continue;
            }
            let is_whitespace = tree.get_node(cid).map_or(false, |node| {
                !node.is_element() && tree.text_content(cid).trim().is_empty()
            });
            if is_whitespace {
                if has_flow_content && pending_whitespace.is_none() {
                    pending_whitespace = Some(cid);
                }
                continue;
            }
            if has_flow_content {
                if let Some(whitespace) = pending_whitespace.take() {
                    flow_dom.push(whitespace);
                }
            }
            flow_dom.push(cid);
            has_flow_content = true;
        }
        let mut row_children: Vec<taffy::NodeId> = flow_dom
            .into_iter()
            .flat_map(|cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
        let right_children: Vec<taffy::NodeId> = right_floats
            .iter()
            .rev()
            .filter_map(|cid| {
                build(
                    tree, *cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
        if !row_children.is_empty() && !right_children.is_empty() {
            let right_group_style = taffy::Style {
                display: taffy::style::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                flex_wrap: taffy::FlexWrap::Wrap,
                margin: taffy::Rect {
                    top: taffy::style::LengthPercentageAuto::length(0.0),
                    right: taffy::style::LengthPercentageAuto::length(0.0),
                    bottom: taffy::style::LengthPercentageAuto::length(0.0),
                    left: taffy::style::LengthPercentageAuto::auto(),
                },
                ..Default::default()
            };
            if let Ok(right_group) =
                taffy_tree.new_with_children(right_group_style, &right_children)
            {
                row_children.push(right_group);
                let row_style = taffy::Style {
                    display: taffy::style::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    flex_wrap: taffy::FlexWrap::Wrap,
                    align_items: Some(taffy::AlignItems::FLEX_START),
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                };
                if let Ok(row) = taffy_tree.new_with_children(row_style, &row_children) {
                    return vec![row];
                }
            }
        }
    }

    let Some(float_idx) = dom_children.iter().position(|&cid| is_float(cid)) else {
        return dom_children
            .iter()
            .flat_map(|&cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
    };

    let mut result: Vec<taffy::NodeId> = dom_children[..float_idx]
        .iter()
        .flat_map(|&cid| {
            build_any(
                tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
            )
        })
        .collect();

    let float_side = styles.get(&dom_children[float_idx]).and_then(|s| s.float);

    // Opposing header floats share one float band even when an empty legacy
    // compatibility box sits between them. This is the classic left-logo /
    // right-tagline header: serializing the two synthetic rows doubles the
    // header height and pushes every later box down. Real float placement
    // scans the same BFC band and puts the second float against the opposite
    // edge when both margin boxes fit.
    let is_empty_bridge = |cid: NodeId| {
        let Some(node) = tree.get_node(cid) else {
            return true;
        };
        if !node.is_element() {
            return tree.text_content(cid).trim().is_empty();
        }
        let style = styles.get(&cid);
        if style.is_some_and(|style| style.display == crate::Display::None) {
            return true;
        }
        let no_size = style
            .map(|style| {
                matches!(style.width, crate::Dimension::Auto)
                    && matches!(style.height, crate::Dimension::Auto)
                    && matches!(style.min_width, crate::Dimension::Auto)
                    && matches!(style.min_height, crate::Dimension::Auto)
                    && matches!(style.max_width, crate::Dimension::Auto)
                    && matches!(style.max_height, crate::Dimension::Auto)
                    && style.margin == crate::Edges::default()
                    && style.padding == crate::Edges::default()
                    && style.padding_percent.iter().all(|value| value.is_none())
                    && style.border == crate::Edges::default()
                    && style.before_pseudo.is_none()
                    && style.after_pseudo.is_none()
            })
            .unwrap_or(true);
        no_size && tree.text_content(cid).trim().is_empty()
    };
    let mut opposite_idx = float_idx + 1;
    while opposite_idx < dom_children.len() && is_empty_bridge(dom_children[opposite_idx]) {
        opposite_idx += 1;
    }
    let opposite_side = dom_children
        .get(opposite_idx)
        .and_then(|cid| styles.get(cid))
        .and_then(|style| (style.display != crate::Display::None).then_some(style.float))
        .flatten();
    if opposite_side.is_some() && opposite_side != float_side {
        let first = build(
            tree,
            dom_children[float_idx],
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        );
        let second = build(
            tree,
            dom_children[opposite_idx],
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        );
        let row_children: Vec<taffy::NodeId> = match float_side {
            Some(crate::Float::Left) => [first, second].into_iter().flatten().collect(),
            _ => [second, first].into_iter().flatten().collect(),
        };
        let row_style = taffy::Style {
            display: taffy::style::Display::Flex,
            flex_direction: taffy::FlexDirection::Row,
            justify_content: Some(taffy::JustifyContent::SPACE_BETWEEN),
            align_items: Some(taffy::AlignItems::FLEX_START),
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        if let Ok(row) = taffy_tree.new_with_children(row_style, &row_children) {
            result.push(row);
        }
        result.extend(dom_children[opposite_idx + 1..].iter().flat_map(|&cid| {
            build_any(
                tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
            )
        }));
        return result;
    }

    // A run of two or more consecutively floated siblings (the classic
    // float-grid idiom: several `float:left; width:N%` boxes forming columns,
    // e.g. craigslist's `.sites .box{float:left;width:23%}` site directory) is
    // not the single-float-beside-flow shape handled below: real float layout
    // places the run side by side, wrapping to a new line when the row fills.
    // Model the run as a wrapping flex row. Whitespace-only text between the
    // floats does not break the run.
    let mut run_end = float_idx + 1;
    let mut float_count = 1usize;
    while run_end < dom_children.len() {
        let cid = dom_children[run_end];
        if is_float(cid) && styles.get(&cid).and_then(|style| style.float) == float_side {
            float_count += 1;
            run_end += 1;
        } else if tree.get_node(cid).map_or(false, |n| !n.is_element())
            && tree.text_content(cid).trim().is_empty()
        {
            run_end += 1;
        } else {
            break;
        }
    }
    if float_count >= 2 {
        let mut run_children: Vec<taffy::NodeId> = dom_children[float_idx..run_end]
            .iter()
            // Formatting whitespace between floats does not generate an
            // in-flow flex item or consume horizontal space.
            .filter(|&&cid| styles.get(&cid).and_then(|s| s.float) == float_side)
            .flat_map(|&cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
        // A common navigation-bar shape is a run of left floats followed by
        // one right float. The right float still scans the current float band:
        // it does not start a new row merely because multiple left floats
        // precede it. Keep the run in one wrapping row and use an auto inline
        // margin to reserve all remaining space before the opposing float.
        //
        // This stays deliberately narrower than a general float manager. In
        // particular, multiple right floats have reverse source-order
        // placement semantics and need their own representation.
        let trailing_right = (float_side == Some(crate::Float::Left)
            && dom_children
                .get(run_end)
                .and_then(|cid| styles.get(cid))
                .and_then(|style| {
                    (style.display != crate::Display::None).then_some(style.float)
                })
                .flatten()
                == Some(crate::Float::Right))
        .then(|| dom_children[run_end]);
        if let Some(right_dom) = trailing_right {
            let right = build(
                tree, right_dom, taffy_tree, id_map, words, engine, ifc_items, styles,
            );
            if let Some(right) = right {
                if let Ok(current) = taffy_tree.style(right) {
                    let mut pushed_right = current.clone();
                    pushed_right.margin.left = taffy::style::LengthPercentageAuto::auto();
                    let _ = taffy_tree.set_style(right, pushed_right);
                }
                run_children.push(right);
                let row_style = taffy::Style {
                    display: taffy::style::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    flex_wrap: taffy::FlexWrap::Wrap,
                    align_items: Some(taffy::AlignItems::FLEX_START),
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                };
                if let Ok(row) = taffy_tree.new_with_children(row_style, &run_children) {
                    result.push(row);
                }
                result.extend(build_children_with_float_zone(
                    tree,
                    parent_id,
                    &dom_children[run_end + 1..],
                    taffy_tree,
                    id_map,
                    words,
                    engine,
                    ifc_items,
                    styles,
                ));
                return result;
            }
        }
        let row_style = taffy::Style {
            display: taffy::style::Display::Flex,
            flex_direction: taffy::FlexDirection::Row,
            flex_wrap: taffy::FlexWrap::Wrap,
            align_items: Some(taffy::AlignItems::FLEX_START),
            // This anonymous row represents the float band's available
            // inline size. Make that size definite before flex line
            // collection: leaving it auto lets intrinsic sizing wrap a set
            // of percentage floats before the parent later stretches the row.
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        if let Ok(row) = taffy_tree.new_with_children(row_style, &run_children) {
            result.push(row);
        }
        result.extend(build_children_with_float_zone(
            tree,
            parent_id,
            &dom_children[run_end..],
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        ));
        return result;
    }

    // Stop growing the zone once the flow siblings collected so far would
    // already fill (an estimate of) the float's own height: real float
    // reflow ends when normal-flow content passes the float's bottom edge,
    // Headings do not terminate a CSS float's influence by themselves.
    // Without this, a short floated thumbnail (a few hundred px) dragged an
    // entire multi-paragraph section into a narrow flow column alongside it
    // — visibly wrong wrapping plus a large empty gap once the (much
    // shorter) float ran out, both from treating "next heading" as the only
    // bound. The estimate is necessarily rough (actual available width is a
    // taffy layout result we don't have yet at tree-build time), but even an
    // approximate bound beats an unbounded one.
    let float_height_budget = estimate_float_height(tree, dom_children[float_idx], styles);
    const ASSUMED_FLOW_WIDTH: f32 = 500.0;
    // `clear` on a sibling ends the zone: the cleared element moves below the
    // float (the clearfix idiom), so it must not join the flow column beside it.
    let clears_this_float = |cid: NodeId| {
        let Some(c) = styles.get(&cid).and_then(|style| {
            (style.display != crate::Display::None)
                .then_some(style.clear)
                .flatten()
        }) else {
            return false;
        };
        match (float_side, c) {
            (_, crate::Clear::Both) => true,
            (Some(crate::Float::Left), crate::Clear::Left) => true,
            (Some(crate::Float::Right), crate::Clear::Right) => true,
            _ => false,
        }
    };
    let mut zone_end = float_idx + 1;
    let mut flow_height_estimate = 0.0f32;
    while zone_end < dom_children.len()
        && !is_float(dom_children[zone_end])
        && !clears_this_float(dom_children[zone_end])
    {
        flow_height_estimate +=
            estimate_flow_sibling_height(tree, dom_children[zone_end], styles, ASSUMED_FLOW_WIDTH);
        zone_end += 1;
        if flow_height_estimate >= float_height_budget {
            break;
        }
    }
    // The float itself is always an element (only elements get style
    // entries, and `is_float` above required one), so a direct `build` call
    // is correct here; only its flow siblings need the word-splitting `build_any`.
    let float_taffy = build(
        tree,
        dom_children[float_idx],
        taffy_tree,
        id_map,
        words,
        engine,
        ifc_items,
        styles,
    );
    // Cap an auto-width float at its widest definite-width descendant so a long
    // wrappable caption cannot inflate it to the caption's one-line max-content
    // width and starve the flow column beside it (the Wikipedia-thumbnail /
    // article-body-collapses-to-one-word-per-line bug). Only when the float is
    // itself auto-width and actually contains such a box; text-only floats
    // (pull quotes, sized infoboxes) are left to normal shrink-to-fit.
    if let Some(float_id) = float_taffy {
        let float_dom = dom_children[float_idx];
        let float_auto = styles
            .get(&float_dom)
            .map(|s| matches!(s.width, crate::Dimension::Auto))
            .unwrap_or(true);
        let float_is_table = styles
            .get(&float_dom)
            .is_some_and(|style| style.is_table_box);
        if float_auto && float_is_table {
            if let Some(w) = max_definite_descendant_width(tree, float_dom, styles) {
                if let Ok(cur) = taffy_tree.style(float_id) {
                    let mut st = cur.clone();
                    // A little slack for the figure's own border/padding so the
                    // image is not clipped; the caption still wraps to ~image width.
                    st.max_size.width = taffy::Dimension::length(w + 12.0);
                    let _ = taffy_tree.set_style(float_id, st);
                }
            }
        }
    }
    match float_taffy {
        Some(float_id) => {
            let flow_column_style = taffy::Style {
                display: taffy::style::Display::Block,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: taffy::Dimension::length(0.0),
                min_size: taffy::Size {
                    width: taffy::Dimension::length(0.0),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            };
            let flow_dom = &dom_children[float_idx + 1..zone_end];
            let flow_column = if flow_dom.is_empty() {
                taffy_tree.new_leaf(flow_column_style).ok()
            } else {
                // The zone is still an ordinary block formatting context:
                // consecutive text/inline siblings must share inline runs,
                // while block siblings stack. Building each sibling directly
                // into a flex column makes every link a separate stretched
                // row. Reuse the mixed-block builder, but leave this anonymous
                // wrapper out of the DOM id map so it cannot overwrite the
                // real parent's rectangle.
                let mut flow_style = styles.get(&parent_id).cloned().unwrap_or_default();
                flow_style.before_content = None;
                flow_style.after_content = None;
                let column = build_mixed_block(
                    tree,
                    parent_id,
                    &flow_style,
                    flow_column_style,
                    flow_dom,
                    taffy_tree,
                    id_map,
                    words,
                    engine,
                    ifc_items,
                    styles,
                );
                if let Some(column_id) = column {
                    id_map.remove(&column_id);
                }
                column
            };

            let row_style = taffy::Style {
                display: taffy::style::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                align_items: Some(taffy::AlignItems::FLEX_START),
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            };
            // A float does not contribute to the height of a non-BFC block
            // that contains it. Put an escaping float inside a zero-height,
            // overflow-visible wrapper: its real width still reserves the
            // current band, but its height can protrude into later descendant
            // blocks of the ancestor BFC. A matching `clear` or any remaining
            // direct full-width sibling keeps the old containing row, since
            // that content explicitly terminates the local float zone.
            let can_escape = zone_end == dom_children.len()
                && styles
                    .get(&parent_id)
                    .map(|style| !establishes_block_formatting_context(style))
                    .unwrap_or(false);
            let row_float = if can_escape {
                let wrapper_style = taffy::Style {
                    display: taffy::style::Display::Block,
                    flex_grow: 0.0,
                    flex_shrink: 0.0,
                    size: taffy::Size {
                        width: taffy::Dimension::auto(),
                        height: taffy::Dimension::length(0.0),
                    },
                    ..Default::default()
                };
                taffy_tree
                    .new_with_children(wrapper_style, &[float_id])
                    .ok()
                    .unwrap_or(float_id)
            } else {
                float_id
            };
            let row_children: Vec<taffy::NodeId> = match float_side {
                Some(crate::Float::Left) => [Some(row_float), flow_column]
                    .into_iter()
                    .flatten()
                    .collect(),
                _ => [flow_column, Some(row_float)]
                    .into_iter()
                    .flatten()
                    .collect(),
            };
            if let Ok(row) = taffy_tree.new_with_children(row_style, &row_children) {
                result.push(row);
                if can_escape {
                    if let (Some(flow), Some(side)) = (flow_column, float_side) {
                        ifc_items.float_continuations.push(FloatContinuation {
                            owner: parent_id,
                            float: float_id,
                            flow,
                            side,
                        });
                    }
                }
            }
        }
        // The float itself failed to build (e.g. display:none resolved for
        // it specifically); still build its flow siblings so their content
        // is not silently lost.
        None => result.extend(
            dom_children[float_idx + 1..zone_end]
                .iter()
                .flat_map(|&cid| {
                    build_any(
                        tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                    )
                }),
        ),
    }

    result.extend(dom_children[zone_end..].iter().flat_map(|&cid| {
        build_any(
            tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
        )
    }));
    result
}
