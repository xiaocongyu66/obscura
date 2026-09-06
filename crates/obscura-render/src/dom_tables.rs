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


use crate::dom_counters::resolve_css_counters;
use crate::dom_invalidation::{add_container_query_reset_scopes, retained_style_plan, RetainedStylePlan};
use crate::dom_sticky::{DerivedGeometryState, DerivedLayoutState, StickyFrame, StickyLayout};
use crate::{to_taffy_style, Rect};

use super::*;
use taffy::style::Dimension;
use crate::dom::{FixedTableColumn, IfcRegistry, build, flatten_contents_children, rendered_children, rendered_descendants, rendered_parent};


/// Give each `<tr>`/`<tbody>`/`<thead>`/`<tfoot>` its CSS table-grid band. In
/// the grid table model these wrappers are not taffy boxes, so without this
/// their backgrounds, borders, and DOM geometry would disappear.
///
/// A row's inline extent includes all cells that start in it, but its block
/// extent must ignore the portion of a `rowspan` that continues through later
/// rows. Nested-table cells must not participate at all. Sections are then the
/// union of their already-synthesized direct rows.
pub(crate) fn synthesize_row_rects(tree: &DomTree, rects: &mut HashMap<NodeId, Rect>) {
    let mut rows = Vec::new();
    let mut sections = Vec::new();
    let mut table_inline: HashMap<NodeId, Rect> = HashMap::new();
    for id in rendered_descendants(tree, tree.document()) {
        let local = match tree
            .get_node(id)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()))
        {
            Some(l) => l,
            None => continue,
        };
        match local.as_str() {
            "tr" => rows.push(id),
            "tbody" | "thead" | "tfoot" => sections.push(id),
            "td" | "th" => {
                let mut ancestor = rendered_parent(tree, id);
                while let Some(parent) = ancestor {
                    let is_table = tree.get_node(parent).map_or(false, |node| {
                        node.as_element()
                            .map_or(false, |element| element.local.as_ref() == "table")
                    });
                    if is_table {
                        if let Some(rect) = rects.get(&id) {
                            table_inline
                                .entry(parent)
                                .and_modify(|current| *current = current.union(rect))
                                .or_insert(*rect);
                        }
                        break;
                    }
                    ancestor = rendered_parent(tree, parent);
                }
            }
            _ => {}
        }
    }

    for id in rows {
        if rects.contains_key(&id) {
            continue;
        }
        let mut inline: Option<Rect> = None;
        let mut block: Option<Rect> = None;
        for cell in tree.children(id) {
            let is_cell = tree
                .get_node(cell)
                .and_then(|n| {
                    n.as_element()
                        .map(|e| matches!(e.local.as_ref(), "td" | "th"))
                })
                .unwrap_or(false);
            if !is_cell {
                continue;
            }
            let Some(r) = rects.get(&cell) else {
                continue;
            };
            inline = Some(match inline {
                Some(a) => a.union(r),
                None => *r,
            });
            let spans_one_row = tree
                .get_node(cell)
                .and_then(|node| {
                    node.get_attribute("rowspan")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .map_or(true, |span| span == 1);
            if spans_one_row {
                block = Some(match block {
                    Some(a) => a.union(r),
                    None => *r,
                });
            }
        }
        if let Some(mut inline) = inline {
            let mut ancestor = rendered_parent(tree, id);
            while let Some(parent) = ancestor {
                if let Some(table_band) = table_inline.get(&parent) {
                    inline.x = table_band.x;
                    inline.width = table_band.width;
                    break;
                }
                ancestor = rendered_parent(tree, parent);
            }
            let block = block.unwrap_or(inline);
            rects.insert(
                id,
                Rect {
                    x: inline.x,
                    y: block.y,
                    width: inline.width,
                    height: block.height,
                },
            );
        }
    }

    for id in sections {
        if rects.contains_key(&id) {
            continue;
        }
        let mut section: Option<Rect> = None;
        for row in tree.children(id) {
            let is_row = tree.get_node(row).map_or(false, |node| {
                node.as_element()
                    .map_or(false, |element| element.local.as_ref() == "tr")
            });
            if !is_row {
                continue;
            }
            if let Some(rect) = rects.get(&row) {
                section = Some(match section {
                    Some(current) => current.union(rect),
                    None => *rect,
                });
            }
        }
        if let Some(rect) = section {
            rects.insert(id, rect);
        }
    }
}

/// Number of ancestor tables containing `id`. Table sizing runs outer-first:
/// an outer table consumes an inner table's intrinsic contributions, then a
/// root layout establishes the inner table's real cell containing block.
pub(crate) fn table_ancestor_depth(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> usize {
    let mut depth = 0usize;
    let mut cur = id;
    while let Some(p) = rendered_parent(tree, cur) {
        if styles.get(&p).is_some_and(|style| style.is_table_box) {
            depth += 1;
        }
        cur = p;
        if depth > 4096 {
            break;
        }
    }
    depth
}

/// Resolve a content-box width without running layout when the complete
/// containing-block chain is ordinary block flow. Flex/grid allocation,
/// floats, positioned boxes, inline-blocks, and intrinsic sizing need a real
/// Taffy pass and deliberately return `None`.
pub(crate) fn reliable_normal_flow_content_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
    depth: usize,
) -> Option<f32> {
    if depth > 4096 {
        return None;
    }
    let style = styles.get(&id)?;
    if style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
        || style.is_inline_block
        || style.width_fit_content
        || style.size_expressions[0].is_some()
    {
        return None;
    }

    let mut parent = rendered_parent(tree, id);
    while parent.is_some_and(|parent_id| {
        styles
            .get(&parent_id)
            .is_some_and(|parent_style| parent_style.display_contents)
    }) {
        parent = parent.and_then(|parent_id| rendered_parent(tree, parent_id));
    }
    let containing_width = if let Some(parent_id) = parent {
        if let Some(parent_style) = styles.get(&parent_id) {
            if matches!(
                parent_style.display,
                crate::Display::Flex | crate::Display::Grid
            ) || parent_style.internal_flex_container
                || parent_style.column_count.is_some()
            {
                return None;
            }
            reliable_normal_flow_content_width(
                tree,
                parent_id,
                styles,
                initial_cb_width,
                depth + 1,
            )?
        } else {
            initial_cb_width
        }
    } else {
        initial_cb_width
    };

    let horizontal_edges =
        style.padding.left + style.padding.right + style.border.left + style.border.right;
    let declared_content = |dimension: crate::Dimension| match dimension {
        crate::Dimension::Px(value) => Some(if style.box_sizing == crate::BoxSizing::ContentBox {
            value
        } else {
            (value - horizontal_edges).max(0.0)
        }),
        crate::Dimension::Percent(percent) => {
            let value = containing_width * percent;
            Some(if style.box_sizing == crate::BoxSizing::ContentBox {
                value
            } else {
                (value - horizontal_edges).max(0.0)
            })
        }
        _ => None,
    };
    let mut content_width = declared_content(style.width).unwrap_or_else(|| {
        (containing_width - style.margin.left - style.margin.right - horizontal_edges).max(0.0)
    });
    if let Some(minimum) = declared_content(style.min_width) {
        content_width = content_width.max(minimum);
    }
    if let Some(maximum) = declared_content(style.max_width) {
        content_width = content_width.min(maximum);
    }
    Some(content_width.max(0.0))
}

/// Resolve an authored definite width even when the box's placement is owned
/// by float or positioned layout. A pixel width is independent of placement;
/// a percentage is definite only when the containing block can itself be
/// resolved without layout. Auto widths and functional size expressions stay
/// on the ordinary Taffy path.
pub(crate) fn reliable_declared_content_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
    depth: usize,
) -> Option<f32> {
    if depth > 4096 {
        return None;
    }
    let style = styles.get(&id)?;
    if style.width_fit_content
        || style.size_expressions[0].is_some()
        || style.size_expressions[2].is_some()
        || style.size_expressions[4].is_some()
    {
        return None;
    }

    let needs_containing_width = matches!(
        style.width,
        crate::Dimension::Percent(_)
    ) || matches!(style.min_width, crate::Dimension::Percent(_))
        || matches!(style.max_width, crate::Dimension::Percent(_));
    let containing_width = needs_containing_width.then(|| {
        let mut parent = rendered_parent(tree, id);
        while parent.is_some_and(|parent_id| {
            styles
                .get(&parent_id)
                .is_some_and(|parent_style| parent_style.display_contents)
        }) {
            parent = parent.and_then(|parent_id| rendered_parent(tree, parent_id));
        }
        if let Some(parent_id) = parent {
            reliable_normal_flow_content_width(
                tree,
                parent_id,
                styles,
                initial_cb_width,
                depth + 1,
            )
            .or_else(|| {
                reliable_declared_content_width(
                    tree,
                    parent_id,
                    styles,
                    initial_cb_width,
                    depth + 1,
                )
            })
        } else {
            Some(initial_cb_width)
        }
    });
    let containing_width = match containing_width {
        Some(Some(width)) => Some(width),
        Some(None) => return None,
        None => None,
    };

    let horizontal_edges =
        style.padding.left + style.padding.right + style.border.left + style.border.right;
    let declared_content = |dimension: crate::Dimension| match dimension {
        crate::Dimension::Px(value) => Some(if style.box_sizing == crate::BoxSizing::ContentBox {
            value
        } else {
            (value - horizontal_edges).max(0.0)
        }),
        crate::Dimension::Percent(percent) => {
            let value = containing_width? * percent;
            Some(if style.box_sizing == crate::BoxSizing::ContentBox {
                value
            } else {
                (value - horizontal_edges).max(0.0)
            })
        }
        _ => None,
    };
    let mut content_width = declared_content(style.width)?;
    if let Some(minimum) = declared_content(style.min_width) {
        content_width = content_width.max(minimum);
    }
    if let Some(maximum) = declared_content(style.max_width) {
        content_width = content_width.min(maximum);
    }
    Some(content_width.max(0.0))
}

/// Resolve the containing-block content width used by CSS's ratio-only
/// auto/auto replaced sizing branch. Decoration-free inline ancestors do not
/// establish containing blocks, so skip them (and display:contents) before
/// using the existing conservative ordinary-flow width resolver. A definite
/// authored width is also safe for floated or positioned containing boxes;
/// their auto-sized allocations still return `None` and remain layout-owned.
pub(crate) fn reliable_ratio_only_available_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
) -> Option<f32> {
    let image = styles.get(&id)?;
    let mut parent = rendered_parent(tree, id);
    while let Some(parent_id) = parent {
        let Some(parent_style) = styles.get(&parent_id) else {
            parent = rendered_parent(tree, parent_id);
            continue;
        };
        if parent_style.display_contents
            || (parent_style.display == crate::Display::Inline
                && !parent_style.is_inline_block)
        {
            parent = rendered_parent(tree, parent_id);
            continue;
        }
        break;
    }

    let containing_width = if let Some(parent_id) = parent {
        reliable_normal_flow_content_width(tree, parent_id, styles, initial_cb_width, 0)
            .or_else(|| {
                reliable_declared_content_width(tree, parent_id, styles, initial_cb_width, 0)
            })?
    } else {
        initial_cb_width
    };
    let horizontal_edges =
        image.padding.left + image.padding.right + image.border.left + image.border.right;
    Some(
        (containing_width
            - image.margin.left
            - image.margin.right
            - horizontal_edges)
            .max(0.0),
    )
}

pub(crate) fn reliable_table_available_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
) -> Option<f32> {
    let table_style = styles.get(&id)?;
    if table_style.float.is_some()
        || matches!(table_style.position, Some(taffy::Position::Absolute))
        || table_style.is_inline_block
    {
        return None;
    }
    let mut parent = rendered_parent(tree, id);
    while parent.is_some_and(|parent_id| {
        styles
            .get(&parent_id)
            .is_some_and(|parent_style| parent_style.display_contents)
    }) {
        parent = parent.and_then(|parent_id| rendered_parent(tree, parent_id));
    }
    let containing_width = match parent {
        Some(parent_id) if styles.contains_key(&parent_id) => {
            let parent_style = styles.get(&parent_id)?;
            if matches!(
                parent_style.display,
                crate::Display::Flex | crate::Display::Grid
            ) || parent_style.internal_flex_container
                || parent_style.column_count.is_some()
            {
                return None;
            }
            reliable_normal_flow_content_width(tree, parent_id, styles, initial_cb_width, 0)?
        }
        _ => initial_cb_width,
    };
    Some((containing_width - table_style.margin.left - table_style.margin.right).max(0.0))
}

/// Resolve the `width: fit-content` keyword after the containing inline space
/// is known.
///
/// Blink's shrink-to-fit helper and Gecko's `ShrinkISizeToFit` use the CSS
/// intrinsic-size formula:
///
/// `max(min-content, min(max-content, available - inline margins))`.
///
/// Taffy's box-size `Dimension` has no intrinsic keyword, so these nodes are
/// initially built as `width:auto`. That preliminary layout is useful: for a
/// stretched grid item it exposes the item's actual grid-area width (which can
/// be much narrower than the grid container), and for a normal block it
/// exposes its fill-available width. We snapshot that available space, measure
/// the subtree at min/max-content, then install the resulting definite
/// preferred width before the final root layout.
pub(crate) fn apply_fit_content_widths<F>(
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
    mut intrinsic_width: F,
) -> bool
where
    F: FnMut(&mut TaffyTree<usize>, taffy::NodeId, taffy::AvailableSpace) -> Option<f32>,
{
    struct Candidate {
        node: taffy::NodeId,
        available: f32,
        margin: f32,
        inline_edges: f32,
        content_box: bool,
    }

    // Snapshot every containing-space input before intrinsic subtree
    // measurements overwrite cached node layouts.
    let candidates: Vec<Candidate> = id_map
        .iter()
        .filter_map(|(&node, &dom)| {
            let style = styles.get(&dom)?;
            if !style.width_fit_content || style.ignores_used_box_sizes() {
                return None;
            }
            let layout = taffy_tree.layout(node).ok()?;
            let margin = layout.margin.left + layout.margin.right;
            let inline_edges = layout.padding.left
                + layout.padding.right
                + layout.border.left
                + layout.border.right;

            let parent = taffy_tree.parent(node);
            let parent_content = parent
                .and_then(|parent| taffy_tree.layout(parent).ok())
                .map(|layout| layout.content_box_width())
                .unwrap_or(initial_cb_width)
                .max(0.0);

            // `auto` stretches in these exact inline-axis situations, so its
            // preliminary margin-box width is the local available space. In
            // particular this preserves a grid area's track width instead of
            // incorrectly using the entire grid container.
            let uses_preliminary_stretch = parent
                .and_then(|parent| taffy_tree.style(parent).ok())
                .map(|parent_style| match parent_style.display {
                    taffy::Display::Block => {
                        style.float.is_none()
                            && !matches!(style.position, Some(taffy::Position::Absolute))
                    }
                    taffy::Display::Grid => {
                        let child_style = taffy_tree.style(node).ok();
                        let horizontal = child_style
                            .and_then(|child| child.justify_self)
                            .or(parent_style.justify_items)
                            .unwrap_or(taffy::AlignItems::NORMAL);
                        let vertical = child_style
                            .and_then(|child| child.align_self)
                            .or(parent_style.align_items)
                            .unwrap_or(taffy::AlignItems::NORMAL);
                        let normal = if child_style.is_some_and(|child| child.item_is_replaced)
                            || (child_style.is_some_and(|child| child.aspect_ratio.is_some())
                                && vertical == taffy::AlignItems::STRETCH)
                        {
                            taffy::AlignItems::START
                        } else {
                            taffy::AlignItems::STRETCH
                        };
                        let child_align = horizontal.resolve_normal(normal);
                        child_align == taffy::AlignItems::STRETCH
                    }
                    taffy::Display::Flex
                        if matches!(
                            parent_style.flex_direction,
                            taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse
                        ) =>
                    {
                        let child_align = taffy_tree
                            .style(node)
                            .ok()
                            .and_then(|child| child.align_self)
                            .or(parent_style.align_items)
                            .unwrap_or(taffy::AlignItems::NORMAL)
                            .resolve_normal(taffy::AlignItems::STRETCH);
                        child_align == taffy::AlignItems::STRETCH
                    }
                    _ => false,
                })
                .unwrap_or(false);
            let available = if uses_preliminary_stretch {
                (layout.size.width + margin).max(0.0)
            } else {
                parent_content
            };

            Some(Candidate {
                node,
                available,
                margin,
                inline_edges,
                content_box: style.box_sizing == crate::BoxSizing::ContentBox,
            })
        })
        .collect();

    let mut changed = false;
    for candidate in candidates {
        let Some(min_content) = intrinsic_width(
            taffy_tree,
            candidate.node,
            taffy::AvailableSpace::MinContent,
        ) else {
            continue;
        };
        let Some(max_content) = intrinsic_width(
            taffy_tree,
            candidate.node,
            taffy::AvailableSpace::MaxContent,
        ) else {
            continue;
        };
        let fill = (candidate.available - candidate.margin).max(0.0);
        let used_outer = min_content.max(max_content.min(fill));
        let declaration = if candidate.content_box {
            (used_outer - candidate.inline_edges).max(0.0)
        } else {
            used_outer.max(0.0)
        };
        let Ok(current) = taffy_tree.style(candidate.node) else {
            continue;
        };
        let mut resolved = current.clone();
        resolved.size.width = taffy::Dimension::length(declaration);
        if taffy_tree.set_style(candidate.node, resolved).is_ok() {
            changed = true;
        }
    }
    changed
}

/// Collect rows together with the exclusive end of their originating row
/// group. HTML rowspans never cross a thead/tbody/tfoot boundary; `rowspan=0`
/// means the remainder of that group, not the remainder of the whole table.
pub(crate) fn collect_table_rows(tree: &DomTree, id: NodeId, rows: &mut Vec<(NodeId, usize)>) {
    let mut direct_start: Option<usize> = None;
    for cid in tree.children(id) {
        let local = tree
            .get_node(cid)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()));
        match local.as_deref() {
            Some("tr") => {
                direct_start.get_or_insert(rows.len());
                rows.push((cid, 0));
            }
            Some("thead") | Some("tbody") | Some("tfoot") => {
                if let Some(start) = direct_start.take() {
                    let end = rows.len();
                    for entry in &mut rows[start..end] {
                        entry.1 = end;
                    }
                }
                let start = rows.len();
                for row in tree.children(cid) {
                    if tree.get_node(row).is_some_and(|node| {
                        node.as_element()
                            .is_some_and(|element| element.local.as_ref() == "tr")
                    }) {
                        rows.push((row, 0));
                    }
                }
                let end = rows.len();
                for entry in &mut rows[start..end] {
                    entry.1 = end;
                }
            }
            _ => {}
        }
    }
    if let Some(start) = direct_start {
        let end = rows.len();
        for entry in &mut rows[start..end] {
            entry.1 = end;
        }
    }
}

/// Effective separate-border spacing. Collapsed tables contribute no spacing
/// at either the outer table edges or between tracks.
pub(crate) fn table_spacing(style: &crate::LayoutStyle) -> (f32, f32) {
    if style.border_collapse.unwrap_or(false) {
        (0.0, 0.0)
    } else {
        style.border_spacing.unwrap_or((0.0, 0.0))
    }
}

/// Horizontal non-track area in the table's border box, excluding the gaps
/// *between* columns: authored border/padding plus one border-spacing unit at
/// each outer edge.
pub(crate) fn table_inline_outer_edges(style: &crate::LayoutStyle) -> f32 {
    let (spacing, _) = table_spacing(style);
    style.border.left
        + style.border.right
        + style.padding.left
        + style.padding.right
        + spacing * 2.0
}

/// Minimum track width implied by percentage columns in an auto-width table.
///
/// Percentage table-cell widths participate in the table's intrinsic width.
/// For example, if a 50% column has a 100px minimum next to 100px of auto
/// columns, the table needs 200px before both constraints can hold. When the
/// constraints cannot all fit (a 100% cell plus any non-empty neighbour), the
/// intrinsic requirement is unbounded and the used width is capped by the
/// available containing-block width. This is the behavior Bootstrap input
/// groups rely on to make an otherwise auto-width `display:table` fill their
/// form while reserving intrinsic space for narrow addon/button cells.
pub(crate) fn auto_table_percentage_intrinsic_floor(minimums: &[f32], percentages: &[Option<f32>]) -> f32 {
    if minimums.len() != percentages.len() || minimums.is_empty() {
        return 0.0;
    }

    let mut remaining = 1.0f32;
    let mut auto_minimum = 0.0f32;
    let mut required = 0.0f32;
    for (minimum, percentage) in minimums.iter().zip(percentages) {
        let minimum = minimum.max(0.0);
        let Some(percentage) = percentage else {
            auto_minimum += minimum;
            continue;
        };
        let effective = percentage.max(0.0).min(remaining);
        remaining = (remaining - effective).max(0.0);
        if minimum > 0.0 {
            if effective <= f32::EPSILON {
                return f32::INFINITY;
            }
            required = required.max(minimum / effective);
        }
    }
    if auto_minimum > 0.0 {
        if remaining <= f32::EPSILON {
            return f32::INFINITY;
        }
        required = required.max(auto_minimum / remaining);
    }
    required
}

/// Resolve auto-table column constraints into final track widths.
///
/// Gecko's auto-table strategy uses four monotonic guesses: all columns at
/// min-content; percentage columns raised toward their percentage; fixed
/// columns raised toward their preferred lengths; then auto columns raised to
/// max-content. The used width interpolates only the category between the two
/// surrounding guesses. This matters for Bootstrap input groups: a 100% input
/// column must shrink by the intrinsic widths of adjacent `width:1%; nowrap`
/// addon/button columns instead of making the table overflow.
pub(crate) fn distribute_auto_table_columns(
    target: f32,
    minimums: &[f32],
    preferreds: &[f32],
    fixed: &[Option<f32>],
    percentages: &[Option<f32>],
) -> Vec<f32> {
    let count = minimums.len();
    if count == 0
        || preferreds.len() != count
        || fixed.len() != count
        || percentages.len() != count
    {
        return minimums.to_vec();
    }
    let target = target.max(0.0);
    let mins: Vec<f32> = minimums.iter().map(|value| value.max(0.0)).collect();
    let prefs: Vec<f32> = preferreds
        .iter()
        .zip(&mins)
        .map(|(preferred, minimum)| preferred.max(*minimum))
        .collect();

    // Percentage column constraints are cumulatively clamped to 100% in
    // source order. Keep zero-valued entries as percentage columns: their
    // intrinsic minimum still participates in the percentage guess.
    let mut remaining = 1.0f32;
    let effective_percentages: Vec<Option<f32>> = percentages
        .iter()
        .map(|percentage| {
            percentage.map(|value| {
                let used = value.max(0.0).min(remaining);
                remaining = (remaining - used).max(0.0);
                used
            })
        })
        .collect();

    let guess_min = mins.clone();
    let guess_min_pct: Vec<f32> = (0..count)
        .map(|index| {
            effective_percentages[index]
                .map(|percentage| (percentage * target).max(mins[index]))
                .unwrap_or(mins[index])
        })
        .collect();
    let guess_min_spec: Vec<f32> = (0..count)
        .map(|index| {
            if effective_percentages[index].is_some() {
                guess_min_pct[index]
            } else if fixed[index].is_some() {
                prefs[index]
            } else {
                mins[index]
            }
        })
        .collect();
    let guess_pref: Vec<f32> = (0..count)
        .map(|index| {
            if effective_percentages[index].is_some() {
                guess_min_pct[index]
            } else {
                prefs[index]
            }
        })
        .collect();
    let total = |values: &[f32]| values.iter().sum::<f32>();
    let min_total = total(&guess_min);
    let min_pct_total = total(&guess_min_pct);
    let min_spec_total = total(&guess_min_spec);
    let pref_total = total(&guess_pref);

    let interpolate = |lower: &[f32], upper: &[f32], wanted: f32| {
        let lower_total = total(lower);
        let delta_total = (total(upper) - lower_total).max(0.0);
        if delta_total <= f32::EPSILON {
            return lower.to_vec();
        }
        let scale = ((wanted - lower_total) / delta_total).clamp(0.0, 1.0);
        lower
            .iter()
            .zip(upper)
            .map(|(low, high)| low + (high - low).max(0.0) * scale)
            .collect()
    };

    if target <= min_total {
        return guess_min;
    }
    if target < min_pct_total {
        return interpolate(&guess_min, &guess_min_pct, target);
    }
    if target < min_spec_total {
        return interpolate(&guess_min_pct, &guess_min_spec, target);
    }
    if target < pref_total {
        return interpolate(&guess_min_spec, &guess_pref, target);
    }

    let mut result = guess_pref;
    let extra = target - pref_total;
    if extra <= f32::EPSILON {
        return result;
    }
    let mut candidates: Vec<(usize, f32)> = (0..count)
        .filter(|index| effective_percentages[*index].is_none() && fixed[*index].is_none())
        .filter_map(|index| (prefs[index] > 0.0).then_some((index, prefs[index])))
        .collect();
    if candidates.is_empty() {
        candidates = (0..count)
            .filter(|index| {
                effective_percentages[*index].is_none() && fixed[*index].is_none()
            })
            .map(|index| (index, 1.0))
            .collect();
    }
    if candidates.is_empty() {
        candidates = (0..count)
            .filter(|index| effective_percentages[*index].is_none() && fixed[*index].is_some())
            .map(|index| (index, prefs[index].max(0.0)))
            .collect();
    }
    if candidates.is_empty() {
        candidates = effective_percentages
            .iter()
            .enumerate()
            .filter_map(|(index, percentage)| {
                (*percentage)
                    .filter(|value| *value > 0.0)
                    .map(|value| (index, value))
            })
            .collect();
    }
    if candidates.is_empty() {
        candidates = (0..count).map(|index| (index, 1.0)).collect();
    }
    let weight: f32 = candidates.iter().map(|(_, value)| *value).sum();
    let candidate_count = candidates.len() as f32;
    for (index, value) in candidates {
        result[index] += if weight > 0.0 {
            extra * value / weight
        } else {
            extra / candidate_count
        };
    }
    result
}

/// Resolve CSS 2 fixed-layout column constraints into exact track widths.
///
/// Columns and first-row cells establish the initial widths. Unspecified
/// tracks share the remaining space; when every track is specified, surplus
/// follows Gecko's fixed-length, percentage, then equal fallback order. A
/// fixed-length over-constraint grows the table, while percentage constraints
/// may shrink proportionally to keep the specified table width.
pub(crate) fn distribute_fixed_table_columns(target: f32, columns: &[FixedTableColumn]) -> Vec<f32> {
    if columns.is_empty() {
        return Vec::new();
    }
    let target = target.max(0.0);
    let mut widths: Vec<f32> = columns
        .iter()
        .map(|column| {
            if column.specified {
                column.length.max(0.0) + column.percentage.max(0.0) * target
            } else {
                0.0
            }
        })
        .collect();
    let mut total: f32 = widths.iter().sum();

    // Percentage columns are the flexible part of an over-constrained fixed
    // table. Never shrink the absolute component: if lengths alone do not fit,
    // the table's used width grows to contain them.
    if total > target {
        let percentage_total: f32 = columns
            .iter()
            .map(|column| column.percentage.max(0.0) * target)
            .sum();
        let shrink = (total - target).min(percentage_total);
        if shrink > 0.0 && percentage_total > 0.0 {
            for (width, column) in widths.iter_mut().zip(columns) {
                let contribution = column.percentage.max(0.0) * target;
                *width = (*width - shrink * contribution / percentage_total).max(0.0);
            }
            total -= shrink;
        }
    }

    let remaining = (target - total).max(0.0);
    if remaining <= f32::EPSILON {
        return widths;
    }
    let unresolved: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| (!column.specified).then_some(index))
        .collect();
    if !unresolved.is_empty() {
        let share = remaining / unresolved.len() as f32;
        for index in unresolved {
            widths[index] = share;
        }
        return widths;
    }

    let mut weights: Vec<f32> = columns
        .iter()
        .map(|column| column.length.max(0.0))
        .collect();
    let mut weight: f32 = weights.iter().sum();
    if weight <= f32::EPSILON {
        weights = columns
            .iter()
            .map(|column| column.percentage.max(0.0))
            .collect();
        weight = weights.iter().sum();
    }
    if weight <= f32::EPSILON {
        weights.fill(1.0);
        weight = columns.len() as f32;
    }
    for (width, column_weight) in widths.iter_mut().zip(weights) {
        *width += remaining * column_weight / weight;
    }
    widths
}

/// Build a `<table>` as a CSS grid. Modeling the table as a grid is what makes
/// columns negotiate a shared width across every row (min-content/max-content
/// track sizing), which the old flex-row-per-`<tr>` stack could not do: each
/// row sized its cells independently, so columns drifted and never lined up.
/// Cells (`<td>`/`<th>`) become grid items placed by (row, column) with
/// colspan/rowspan mapped to grid spans; `<tr>`/`<tbody>`/`<thead>`/`<tfoot>`
/// do not get their own layout boxes (their backgrounds are not modeled yet).
/// The grid node is created at width:auto here; its final width is set later in
/// `layout_dom` by a two-pass intrinsic measurement (see the table-sizing pass)
/// so the table can grow to fit an unbreakable wide cell the way real tables do.
/// Returns `None` (falling back to the generic path) if the table has no cells.
pub(crate) fn build_table(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<taffy::NodeId> {
    let style = styles.get(&id)?;
    let native_html_table = tree.get_node(id).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "table")
    });
    let authored_row_children = if native_html_table {
        None
    } else {
        let children = rendered_children(tree, id);
        let mut flattened = Vec::new();
        flatten_contents_children(tree, &children, styles, &mut flattened);
        // Full anonymous-table fixup is not represented yet. Falling back to
        // ordinary box construction preserves every child; partially taking
        // the dedicated path would silently discard direct text/non-cell
        // boxes once one real table cell made the build succeed.
        let all_rendered_children_are_cells = flattened.iter().all(|child| {
            let hidden = styles
                .get(child)
                .is_some_and(|child_style| child_style.display == crate::Display::None);
            let ignorable_whitespace = tree.get_node(*child).is_some_and(|node| {
                !node.is_element() && tree.text_content(*child).trim().is_empty()
            });
            hidden
                || ignorable_whitespace
                || styles
                    .get(child)
                    .is_some_and(|child_style| child_style.is_table_cell_box)
        });
        if !all_rendered_children_are_cells {
            return None;
        }
        Some(flattened)
    };
    let mut rows: Vec<(NodeId, usize)> = Vec::new();
    if native_html_table {
        collect_table_rows(tree, id, &mut rows);
        if rows.is_empty() {
            return None;
        }
    } else {
        // CSS table fixup inserts an anonymous row around table-cell children
        // that are direct children of a table. The grid representation does
        // not need a material row node, but retaining one logical row gives
        // those cells the same shared-track negotiation as native cells.
        rows.push((id, 1));
    }
    // Bounds so a crafted table cannot exhaust memory or time: `colspan`/
    // `rowspan` and the column count are page-controlled, and the occupancy
    // fill is O(rows x cols) native code that neither the V8 watchdog nor a
    // tokio timeout can interrupt. Real tables never approach these. Rows are
    // capped too, both to bound work and to keep grid line/span indices within
    // taffy's i16/u16 range.
    const MAX_SPAN: usize = 1000;
    const MAX_COLS: usize = 1024;
    const MAX_ROWS: usize = 10000;
    if rows.len() > MAX_ROWS {
        rows.truncate(MAX_ROWS);
    }
    let nrows = rows.len();

    // Assign every cell a (row, column) with a rowspan-occupancy grid so a cell
    // that spans down pushes later rows' cells past the columns it still covers.
    let span_attr = |cid: NodeId, name: &str| -> usize {
        tree.get_node(cid)
            .and_then(|n| {
                n.get_attribute(name)
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(1)
    };
    let mut occupied: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut placed: Vec<(NodeId, usize, usize, usize, usize)> = Vec::new();
    let mut ncols = 0usize;
    for (r, &(tr, group_end)) in rows.iter().enumerate() {
        let mut c = 0usize;
        let row_children = if native_html_table {
            tree.children(tr)
        } else {
            authored_row_children.clone().unwrap_or_default()
        };
        for cid in row_children {
            let local = tree
                .get_node(cid)
                .and_then(|n| n.as_element().map(|e| e.local.to_string()));
            let is_cell = if native_html_table {
                matches!(local.as_deref(), Some("td") | Some("th"))
            } else {
                styles
                    .get(&cid)
                    .is_some_and(|cell| cell.is_table_cell_box)
            };
            if !is_cell {
                continue;
            }
            // A hidden cell is removed from the table model entirely (it must
            // not reserve a column slot, or the surviving cells shift right).
            if styles
                .get(&cid)
                .map(|s| s.display == crate::Display::None)
                .unwrap_or(false)
            {
                continue;
            }
            while occupied.contains(&(r, c)) {
                c += 1;
            }
            if c >= MAX_COLS {
                break;
            }
            let cs = if native_html_table {
                span_attr(cid, "colspan").clamp(1, MAX_SPAN)
            } else {
                1
            };
            // rowspan=0 means "span to the end of this row group". Explicit
            // spans are clipped at the same boundary by the effective cell map.
            let rs_raw = if native_html_table {
                span_attr(cid, "rowspan")
            } else {
                1
            };
            let rows_left_in_group = group_end.min(nrows).saturating_sub(r).max(1);
            let rs = if rs_raw == 0 {
                rows_left_in_group
            } else {
                rs_raw
            }
            .clamp(1, rows_left_in_group);
            for dr in 0..rs {
                for dc in 0..cs {
                    occupied.insert((r + dr, c + dc));
                }
            }
            placed.push((cid, r, c, rs, cs));
            c += cs;
            ncols = ncols.max(c).min(MAX_COLS);
        }
    }
    if placed.is_empty() || ncols == 0 {
        return None;
    }

    // Build each cell and pin it to its grid area.
    let mut children: Vec<taffy::NodeId> = Vec::new();
    for (cid, r, c, rs, cs) in &placed {
        let Some(cell_node) = build(
            tree, *cid, taffy_tree, id_map, words, engine, ifc_items, styles,
        ) else {
            continue;
        };
        if let Ok(cur) = taffy_tree.style(cell_node) {
            let mut cstyle = cur.clone();
            cstyle.grid_row = taffy::Line {
                start: line((*r as i16) + 1),
                end: span(*rs as u16),
            };
            cstyle.grid_column = taffy::Line {
                start: line((*c as i16) + 1),
                end: span(*cs as u16),
            };
            // Grid does the sizing; a leftover flex_grow from the flex-table
            // heuristic would be ignored anyway, but clear it to be explicit.
            cstyle.flex_grow = 0.0;
            // A cell's specified width sizes its COLUMN (fed into the track
            // by the pre-pass above); the cell box itself always fills its
            // grid area. Left in place, a `width:50%` cell would shrink to
            // half of its own already-halved track.
            cstyle.size.width = Dimension::auto();
            // Taffy's grid-item automatic minimum is an engine artifact here:
            // the table track has already collected the cell's min-content
            // contribution and owns final column sizing. Remove only the
            // initial `auto` minimum at translation time; an authored
            // min-width remains a real constraint and computed style stays
            // declaration-order independent.
            if styles
                .get(cid)
                .is_some_and(|cell_style| cell_style.min_width == crate::Dimension::Auto)
            {
                cstyle.min_size.width = taffy::Dimension::length(0.0);
            }
            let _ = taffy_tree.set_style(cell_node, cstyle);
        }
        children.push(cell_node);
    }
    if children.is_empty() {
        return None;
    }

    // Column sizing pre-pass: specified widths on `<col>` elements and on
    // colspan-1 cells feed the tracks, so author column sizing actually
    // applies (a `td{width:200px}` must size the COLUMN, across every row).
    // A percent width becomes a percent track (resolving against the table),
    // a px width caps the track at that length (min-content still protects
    // the content), and unspecified columns keep content sizing with an
    // `auto` max so they stretch to fill a definite table width instead of
    // leaving a dead strip of bare table background.
    let mut col_px: Vec<Option<f32>> = vec![None; ncols];
    let mut col_pct: Vec<Option<f32>> = vec![None; ncols];
    let fixed_layout = style.table_layout_fixed
        && matches!(style.width, crate::Dimension::Px(_) | crate::Dimension::Percent(_));
    let mut fixed_columns = vec![FixedTableColumn::default(); ncols];
    let attr_width = |cid: NodeId| -> (Option<f32>, Option<f32>) {
        let Some(v) = tree
            .get_node(cid)
            .and_then(|n| n.get_attribute("width").map(|s| s.trim().to_string()))
        else {
            return (None, None);
        };
        if let Some(p) = v
            .strip_suffix('%')
            .and_then(|s| s.trim().parse::<f32>().ok())
        {
            (None, Some(p / 100.0))
        } else {
            (v.trim_end_matches("px").trim().parse::<f32>().ok(), None)
        }
    };
    let style_width = |cid: NodeId| -> (Option<f32>, Option<f32>) {
        match styles.get(&cid).map(|s| s.width) {
            Some(crate::Dimension::Px(w)) if w > 0.0 => (Some(w), None),
            Some(crate::Dimension::Percent(p)) if p > 0.0 => (None, Some(p)),
            _ => attr_width(cid),
        }
    };
    let fixed_style_width = |cid: NodeId| -> (Option<f32>, Option<f32>) {
        match styles.get(&cid).map(|s| s.width) {
            Some(crate::Dimension::Px(w)) if w >= 0.0 => (Some(w), None),
            Some(crate::Dimension::Percent(p)) if p >= 0.0 => (None, Some(p)),
            _ => attr_width(cid),
        }
    };
    // <col> elements (direct or under <colgroup>), each spanning `span` columns.
    let mut next_col = 0usize;
    let mut col_elems: Vec<NodeId> = Vec::new();
    for cid in tree.children(id) {
        match tree
            .get_node(cid)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()))
            .as_deref()
        {
            Some("col") => col_elems.push(cid),
            Some("colgroup") => {
                for gc in tree.children(cid) {
                    if tree
                        .get_node(gc)
                        .and_then(|n| n.as_element().map(|e| e.local.as_ref() == "col"))
                        .unwrap_or(false)
                    {
                        col_elems.push(gc);
                    }
                }
            }
            _ => {}
        }
    }
    for col_el in &col_elems {
        let span = tree
            .get_node(*col_el)
            .and_then(|n| {
                n.get_attribute("span")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(1)
            .clamp(1, MAX_SPAN);
        let (px, pct) = style_width(*col_el);
        let (fixed_px, fixed_pct) = fixed_style_width(*col_el);
        for _ in 0..span {
            if next_col >= ncols {
                break;
            }
            col_px[next_col] = px;
            col_pct[next_col] = pct;
            if fixed_layout && (fixed_px.is_some() || fixed_pct.is_some()) {
                fixed_columns[next_col] = FixedTableColumn {
                    length: fixed_px.unwrap_or(0.0),
                    percentage: fixed_pct.unwrap_or(0.0),
                    specified: true,
                };
            }
            next_col += 1;
        }
    }
    // colspan-1 cells override <col> (they are closer to the content).
    for (cid, _r, c, _rs, cs) in &placed {
        if *cs != 1 || *c >= ncols {
            continue;
        }
        let (mut px, pct) = style_width(*cid);
        // A fixed width declared on a cell describes its content box unless
        // the author opted into border-box. Grid tracks describe the cell's
        // outer border box, so carry padding and border into the pinned track.
        // `<col>` widths above already describe the column track and must not
        // receive a particular cell's box edges.
        if let (Some(w), Some(s)) = (px, styles.get(cid)) {
            if s.box_sizing == crate::BoxSizing::ContentBox {
                px = Some(w + s.padding.left + s.padding.right + s.border.left + s.border.right);
            }
        }
        if let Some(w) = px {
            col_px[*c] = Some(col_px[*c].map_or(w, |cur| cur.max(w)));
        }
        if let Some(p) = pct {
            col_pct[*c] = Some(col_pct[*c].map_or(p, |cur| cur.max(p)));
        }
    }

    // Under fixed table layout only the first row contributes cell widths,
    // and an explicit `<col>` constraint wins for every covered column. A
    // spanning cell's outer width is split with the inter-column spacing
    // removed, matching Gecko's `((width + spacing) / span) - spacing` rule.
    if fixed_layout {
        let (horizontal_spacing, _) = table_spacing(style);
        for (cid, row, start, _rowspan, colspan) in &placed {
            if *row != 0 || *start >= ncols {
                continue;
            }
            let (px, pct) = fixed_style_width(*cid);
            if px.is_none() && pct.is_none() {
                continue;
            }
            let span = (*colspan).min(ncols - *start).max(1);
            let cell_style = styles.get(cid);
            let edges = cell_style
                .filter(|cell| cell.box_sizing == crate::BoxSizing::ContentBox)
                .map(|cell| {
                    cell.padding.left
                        + cell.padding.right
                        + cell.border.left
                        + cell.border.right
                })
                .unwrap_or(0.0);
            let outer_length = px.unwrap_or(0.0) + edges;
            let per_column_length =
                ((outer_length + horizontal_spacing) / span as f32 - horizontal_spacing)
                    .max(0.0);
            let per_column_percentage = pct.unwrap_or(0.0).max(0.0) / span as f32;
            for column in &mut fixed_columns[*start..*start + span] {
                if !column.specified {
                    *column = FixedTableColumn {
                        length: per_column_length,
                        percentage: per_column_percentage,
                        specified: true,
                    };
                }
            }
        }
    }

    // Row sizing: a `height` on the row or a rowspan-1 cell is a MINIMUM
    // (content can always grow a row), matching how tables treat heights.
    let mut row_min: Vec<Option<f32>> = vec![None; nrows];
    if native_html_table {
        for (r, &(tr, _)) in rows.iter().enumerate() {
            if let Some(crate::Dimension::Px(h)) = styles.get(&tr).map(|s| s.height) {
                if h > 0.0 {
                    row_min[r] = Some(h);
                }
            }
        }
    }
    for (cid, r, _c, rs, _cs) in &placed {
        if *rs != 1 {
            continue;
        }
        if let Some(crate::Dimension::Px(h)) = styles.get(cid).map(|s| s.height) {
            if h > 0.0 {
                row_min[*r] = Some(row_min[*r].map_or(h, |cur| cur.max(h)));
            }
        }
    }

    let col = |i: usize| {
        if fixed_layout {
            return taffy::GridTemplateComponent::Single(taffy::MinMax {
                min: taffy::MinTrackSizingFunction::length(0.0),
                max: taffy::MaxTrackSizingFunction::length(0.0),
            });
        }
        let max = if let Some(p) = col_pct[i] {
            taffy::MaxTrackSizingFunction::percent(p)
        } else if let Some(px) = col_px[i] {
            taffy::MaxTrackSizingFunction::length(px)
        } else {
            taffy::MaxTrackSizingFunction::auto()
        };
        taffy::GridTemplateComponent::Single(taffy::MinMax {
            min: taffy::MinTrackSizingFunction::min_content(),
            max,
        })
    };
    let row_track = |r: usize| {
        let min = match row_min[r] {
            Some(h) => taffy::MinTrackSizingFunction::length(h),
            None => taffy::MinTrackSizingFunction::auto(),
        };
        taffy::GridTemplateComponent::Single(taffy::MinMax {
            min,
            max: taffy::MaxTrackSizingFunction::auto(),
        })
    };
    // In the separate-border model, border-spacing also exists between the
    // table edge and the first/last row and column. Grid `gap` only covers
    // interior tracks, so model the two outer spacing bands as internal
    // layout padding while leaving the computed CSS padding unchanged.
    let (horizontal_spacing, vertical_spacing) = table_spacing(style);
    let mut grid_style = style.clone();
    grid_style.padding.left += horizontal_spacing;
    grid_style.padding.right += horizontal_spacing;
    grid_style.padding.top += vertical_spacing;
    grid_style.padding.bottom += vertical_spacing;
    let mut tstyle = to_taffy_style(&grid_style);
    tstyle.display = taffy::style::Display::Grid;
    // A percentage width resolves against the container, so keep it and let the
    // used-width pass leave it to taffy. Any other width (px or auto) is forced
    // to auto here so that pass can measure content before choosing the width.
    if !matches!(style.width, crate::Dimension::Percent(_)) {
        tstyle.size.width = Dimension::auto();
    }
    tstyle.grid_template_columns = (0..ncols).map(col).collect();
    tstyle.grid_template_rows = (0..nrows).map(row_track).collect();
    tstyle.gap = taffy::Size {
        width: length(horizontal_spacing),
        height: length(vertical_spacing),
    };
    let table_node = taffy_tree.new_with_children(tstyle, &children).ok()?;
    id_map.insert(table_node, id);
    ifc_items.table_rows.insert(table_node, row_min);
    if fixed_layout {
        ifc_items.fixed_table_cols.insert(table_node, fixed_columns);
    }
    if col_px.iter().any(Option::is_some) || col_pct.iter().any(Option::is_some) {
        ifc_items.table_cols.insert(table_node, (col_px, col_pct));
    }
    Some(table_node)
}
