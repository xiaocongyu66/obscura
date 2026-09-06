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
use crate::dom_sticky::{DerivedGeometryState, DerivedLayoutState, StickyFrame, StickyLayout};
use crate::{to_taffy_style, Rect};

use super::*;
use crate::dom::{GeneratedBoxKind, IfcRegistry, rendered_children, rendered_parent, rendered_descendants, establishes_block_formatting_context};

#[derive(Clone, Copy)]
pub(crate) enum EffectiveGridChild {
    Dom(NodeId),
    Generated {
        host: NodeId,
        kind: GeneratedBoxKind,
    },
}

pub(crate) fn collect_effective_grid_children(
    tree: &DomTree,
    children: &[NodeId],
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    out: &mut Vec<EffectiveGridChild>,
) {
    for &child in children {
        let transparent = styles
            .get(&child)
            .is_some_and(|style| style.display_contents && style.display != crate::Display::None);
        if !transparent {
            out.push(EffectiveGridChild::Dom(child));
            continue;
        }
        if styles
            .get(&child)
            .and_then(|style| style.before_pseudo.as_ref())
            .is_some()
        {
            out.push(EffectiveGridChild::Generated {
                host: child,
                kind: GeneratedBoxKind::Before,
            });
        }
        collect_effective_grid_children(tree, &rendered_children(tree, child), styles, out);
        if styles
            .get(&child)
            .and_then(|style| style.after_pseudo.as_ref())
            .is_some()
        {
            out.push(EffectiveGridChild::Generated {
                host: child,
                kind: GeneratedBoxKind::After,
            });
        }
    }
}

pub(crate) fn effective_grid_child_style<'a>(
    child: EffectiveGridChild,
    styles: &'a HashMap<NodeId, crate::LayoutStyle>,
) -> Option<&'a crate::LayoutStyle> {
    match child {
        EffectiveGridChild::Dom(node) => styles.get(&node),
        EffectiveGridChild::Generated { host, kind } => {
            let host = styles.get(&host)?;
            match kind {
                GeneratedBoxKind::Before => host.before_pseudo.as_deref(),
                GeneratedBoxKind::After => host.after_pseudo.as_deref(),
            }
        }
    }
}

pub(crate) fn effective_grid_child_style_mut<'a>(
    child: EffectiveGridChild,
    styles: &'a mut HashMap<NodeId, crate::LayoutStyle>,
) -> Option<&'a mut crate::LayoutStyle> {
    match child {
        EffectiveGridChild::Dom(node) => styles.get_mut(&node),
        EffectiveGridChild::Generated { host, kind } => {
            let host = styles.get_mut(&host)?;
            match kind {
                GeneratedBoxKind::Before => host.before_pseudo.as_deref_mut(),
                GeneratedBoxKind::After => host.after_pseudo.as_deref_mut(),
            }
        }
    }
}

/// Walk the tree; for each `display: grid` element that declares
/// `grid-template-areas`, resolve each box child's `grid-area` name to a taffy
/// line placement. `display:contents` wrappers are transparent here for the
/// same reason they are transparent when the Taffy child list is built.
pub(crate) fn resolve_grid_areas(
    tree: &DomTree,
    root: NodeId,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        for cid in tree.children(id) {
            stack.push(cid);
        }
        let (areas, col_lines, row_lines) = match styles.get(&id) {
            Some(s) if s.display == crate::Display::Grid => (
                s.grid_areas.clone().filter(|a| !a.is_empty()),
                s.grid_col_line_names.clone(),
                s.grid_row_line_names.clone(),
            ),
            _ => continue,
        };
        if areas.is_none() && col_lines.is_none() && row_lines.is_none() {
            continue;
        }
        let mut grid_children = Vec::new();
        collect_effective_grid_children(
            tree,
            &rendered_children(tree, id),
            styles,
            &mut grid_children,
        );

        if let Some(areas) = &areas {
            // name -> (row_start, row_end, col_start, col_end) in 0-based track indices.
            let mut spans: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
            for (r, row) in areas.iter().enumerate() {
                for (c, name) in row.iter().enumerate() {
                    if name == "." {
                        continue;
                    }
                    spans
                        .entry(name.clone())
                        .and_modify(|s| {
                            s.0 = s.0.min(r);
                            s.1 = s.1.max(r);
                            s.2 = s.2.min(c);
                            s.3 = s.3.max(c);
                        })
                        .or_insert((r, r, c, c));
                }
            }

            for child in grid_children.iter().copied() {
                let Some(cstyle) = effective_grid_child_style_mut(child, styles) else {
                    continue;
                };
                let Some(name) = cstyle.grid_area_name.clone() else {
                    continue;
                };
                if let Some(&(r0, r1, c0, c1)) = spans.get(&name) {
                    use taffy::style_helpers::line;
                    cstyle.grid_row = Some(taffy::Line {
                        start: line((r0 + 1) as i16),
                        end: line((r1 + 2) as i16),
                    });
                    cstyle.grid_column = Some(taffy::Line {
                        start: line((c0 + 1) as i16),
                        end: line((c1 + 2) as i16),
                    });
                }
            }
        }

        // Named grid lines: resolve children placed with `grid-column`/`grid-row`
        // values that reference a line name against this container's maps.
        if col_lines.is_some() || row_lines.is_some() {
            for child in grid_children.iter().copied() {
                let Some(cstyle) = effective_grid_child_style_mut(child, styles) else {
                    continue;
                };
                if let (Some(raw), Some(map)) = (cstyle.grid_column_raw.clone(), &col_lines) {
                    if let Some(l) = resolve_named_placement(&raw, map) {
                        cstyle.grid_column = Some(l);
                    }
                }
                if let (Some(raw), Some(map)) = (cstyle.grid_row_raw.clone(), &row_lines) {
                    if let Some(l) = resolve_named_placement(&raw, map) {
                        cstyle.grid_row = Some(l);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ColumnSubgridWrapper {
    node: taffy::NodeId,
    gap: f32,
    start_mbp: f32,
    end_mbp: f32,
}

#[derive(Clone, Copy)]
struct ColumnSubgridLeaf {
    node: taffy::NodeId,
    dom: NodeId,
    column: usize,
    gap: f32,
    start_mbp: f32,
    end_mbp: f32,
}

struct ColumnSubgridPlan {
    parent: taffy::NodeId,
    track_count: usize,
    parent_gap: f32,
    wrappers: Vec<ColumnSubgridWrapper>,
    leaves: Vec<ColumnSubgridLeaf>,
}

pub(crate) fn is_full_span_column_subgrid(
    style: &crate::LayoutStyle,
) -> bool {
    if style.display != crate::Display::Grid
        || !style.grid_template_columns_subgrid
        || style.overflow_hidden
        || style.position == Some(taffy::Position::Absolute)
        || style.width != crate::Dimension::Auto
        || style.justify_self.is_some_and(|value| {
            value.resolve_normal(taffy::AlignSelf::STRETCH) != taffy::AlignSelf::STRETCH
        })
        || style.margin_auto[1]
        || style.margin_auto[3]
    {
        return false;
    }
    let Some(line) = &style.grid_column else {
        return false;
    };
    matches!(
        (&line.start, &line.end),
        (taffy::GridPlacement::Line(start), taffy::GridPlacement::Line(end))
            if start.as_i16() == 1 && end.as_i16() == -1
    )
}

/// Whether an item is wholly eligible for ordinary grid auto-placement.
///
/// The style model retains an explicit `grid-area:auto` as `Some(Line {
/// Auto, Auto })`, while an omitted placement stays `None`. Both represent
/// the same indefinite row and column spans to the grid placement algorithm.
/// Raw named placements are never auto: an unresolved name must not enter the
/// bounded subgrid reduction merely because it has no numeric `Line` yet.
pub(crate) fn has_only_auto_grid_placement(
    style: &crate::LayoutStyle,
) -> bool {
    if style.grid_column_raw.is_some() || style.grid_row_raw.is_some() {
        return false;
    }
    let axis_is_auto = |line: Option<&taffy::Line<taffy::GridPlacement>>| {
        line.is_none_or(|line| {
            matches!(line.start, taffy::GridPlacement::Auto)
                && matches!(line.end, taffy::GridPlacement::Auto)
        })
    };
    axis_is_auto(style.grid_column.as_ref()) && axis_is_auto(style.grid_row.as_ref())
}

/// Collect the deliberately bounded Grid Level 2 subset used by broad
/// "aligned rows" components: a full-span column subgrid, optionally nested
/// through more full-span column subgrids, whose final children auto-place one
/// per inherited column. Partial spans, explicit leaf placement, independent
/// formatting contexts, and authored inline sizing are left on the existing
/// fallback rather than being represented inaccurately.
pub(crate) fn collect_column_subgrid_descendants(
    tree: &DomTree,
    dom: NodeId,
    track_count: usize,
    taffy_by_dom: &HashMap<NodeId, taffy::NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    start_mbp: f32,
    end_mbp: f32,
    depth: usize,
    wrappers: &mut Vec<ColumnSubgridWrapper>,
    leaves: &mut Vec<ColumnSubgridLeaf>,
) -> bool {
    if depth > 8 {
        return false;
    }
    let Some(style) = styles.get(&dom) else {
        return false;
    };
    let Some(&node) = taffy_by_dom.get(&dom) else {
        return false;
    };
    let start_mbp = start_mbp + style.margin.left + style.border.left + style.padding.left;
    let end_mbp = end_mbp + style.margin.right + style.border.right + style.padding.right;
    let gap = style.column_gap.unwrap_or(0.0);
    wrappers.push(ColumnSubgridWrapper {
        node,
        gap,
        start_mbp,
        end_mbp,
    });

    let children: Vec<NodeId> = tree
        .children(dom)
        .into_iter()
        .filter(|child| {
            styles
                .get(child)
                .map(|style| style.display != crate::Display::None)
                .unwrap_or(false)
        })
        .collect();
    if children.is_empty() {
        return false;
    }
    let nested = children
        .iter()
        .filter(|child| styles.get(child).is_some_and(is_full_span_column_subgrid))
        .count();
    if nested > 0 {
        if nested != children.len() {
            return false;
        }
        return children.into_iter().all(|child| {
            collect_column_subgrid_descendants(
                tree,
                child,
                track_count,
                taffy_by_dom,
                styles,
                start_mbp,
                end_mbp,
                depth + 1,
                wrappers,
                leaves,
            )
        });
    }

    let flow = style.grid_auto_flow.unwrap_or(taffy::GridAutoFlow::Row);
    if !matches!(
        flow,
        taffy::GridAutoFlow::Row | taffy::GridAutoFlow::RowDense
    ) {
        return false;
    }
    for (index, child) in children.into_iter().enumerate() {
        let Some(child_style) = styles.get(&child) else {
            return false;
        };
        // This first subset intentionally excludes spanning and explicitly
        // placed items. It is the common data-row/card-row pattern and keeps
        // each descendant contribution attributable to one ancestor track.
        // Explicit `grid-area:auto` remains ordinary auto-placement in both
        // axes and is therefore equivalent to omitting all placement values.
        if !has_only_auto_grid_placement(child_style)
            || child_style.position == Some(taffy::Position::Absolute)
            || child_style.margin_auto[1]
            || child_style.margin_auto[3]
        {
            return false;
        }
        let Some(&node) = taffy_by_dom.get(&child) else {
            return false;
        };
        leaves.push(ColumnSubgridLeaf {
            node,
            dom: child,
            column: index % track_count,
            gap,
            start_mbp,
            end_mbp,
        });
    }
    true
}

pub(crate) fn fixed_grid_tracks(widths: &[f32]) -> Vec<taffy::GridTemplateComponent<String>> {
    widths
        .iter()
        .map(|width| {
            taffy::GridTemplateComponent::Single(taffy::MinMax {
                min: taffy::MinTrackSizingFunction::length((*width).max(0.0)),
                max: taffy::MaxTrackSizingFunction::length((*width).max(0.0)),
            })
        })
        .collect()
}

/// Resolve a safe full-span column-subgrid subset in two passes.
///
/// Gecko first collects subgrid descendants into the nearest non-subgridded
/// ancestor's track sizing, then copies that ancestor's *used* track sizes
/// down the chain. Its descendant contributions add accumulated edge
/// margin/border/padding and center a custom subgrid gap over the ancestor
/// gap. Taffy has no subgrid primitive, so we reproduce those same operations
/// only for definite, all-auto, single-span rows. Once every max-content
/// growth limit fits, `justify-content:normal` stretches all auto tracks by an
/// equal share; narrower/cyclic cases decline this fast path.
///
/// The current style model does not expose orthogonal writing modes. RTL grids
/// are deliberately excluded because this physical-column reduction has not
/// yet been mirrored into logical track coordinates. Percentage-width parents are
/// accepted only after the preliminary layout produced finite, explicit used
/// track sizes; that resolved track sum is the definite basis for pass two.
pub(crate) fn apply_full_span_column_subgrids<F>(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    mut measure_max_content: F,
) -> bool
where
    F: FnMut(&mut TaffyTree<usize>, taffy::NodeId) -> Option<f32>,
{
    let taffy_by_dom: HashMap<NodeId, taffy::NodeId> =
        id_map.iter().map(|(taffy, dom)| (*dom, *taffy)).collect();
    let mut plans = Vec::new();

    for (&dom, style) in styles {
        if style.display != crate::Display::Grid
            || style.direction == Some(taffy::Direction::Rtl)
            || style.grid_template_columns_subgrid
            || !matches!(
                style.width,
                crate::Dimension::Px(_) | crate::Dimension::Percent(_)
            )
            || style
                .justify_content
                .is_some_and(|value| value != taffy::JustifyContent::STRETCH)
        {
            continue;
        }
        let all_auto = !style.grid_template_columns.is_empty()
            && style.grid_template_columns.iter().all(|track| {
                matches!(
                    track,
                    taffy::GridTemplateComponent::Single(size)
                        if size.min.is_auto() && size.max.is_auto()
                )
            });
        if !all_auto || style.grid_template_columns.len() > 32 {
            continue;
        }
        let track_count = style.grid_template_columns.len();
        let direct: Vec<NodeId> = tree
            .children(dom)
            .into_iter()
            .filter(|child| {
                styles
                    .get(child)
                    .is_some_and(|style| style.display != crate::Display::None)
            })
            .collect();
        if direct.is_empty()
            || !direct
                .iter()
                .all(|child| styles.get(child).is_some_and(is_full_span_column_subgrid))
        {
            continue;
        }
        let Some(&parent) = taffy_by_dom.get(&dom) else {
            continue;
        };
        let mut wrappers = Vec::new();
        let mut leaves = Vec::new();
        if !direct.into_iter().all(|child| {
            collect_column_subgrid_descendants(
                tree,
                child,
                track_count,
                &taffy_by_dom,
                styles,
                0.0,
                0.0,
                0,
                &mut wrappers,
                &mut leaves,
            )
        }) || leaves.is_empty()
        {
            continue;
        }
        plans.push(ColumnSubgridPlan {
            parent,
            track_count,
            parent_gap: style.column_gap.unwrap_or(0.0),
            wrappers,
            leaves,
        });
    }

    let mut changed = false;
    for plan in plans {
        let target_track_sum = match taffy_tree.detailed_layout_info(plan.parent) {
            taffy::tree::DetailedLayoutInfo::Grid(info)
                if info.columns.negative_implicit_tracks == 0
                    && info.columns.positive_implicit_tracks == 0
                    && info.columns.explicit_tracks as usize == plan.track_count =>
            {
                info.columns.sizes.iter().sum::<f32>()
            }
            _ => continue,
        };
        if !target_track_sum.is_finite() || target_track_sum <= 0.0 {
            continue;
        }
        let mut max_content = vec![0.0f32; plan.track_count];
        let mut valid = true;
        for leaf in &plan.leaves {
            let Some(mut contribution) = measure_max_content(taffy_tree, leaf.node) else {
                valid = false;
                break;
            };
            let Some(leaf_style) = styles.get(&leaf.dom) else {
                valid = false;
                break;
            };
            contribution += leaf_style.margin.left + leaf_style.margin.right;
            if plan.track_count > 1 {
                let gap_delta = leaf.gap - plan.parent_gap;
                contribution += if leaf.column == 0 || leaf.column + 1 == plan.track_count {
                    gap_delta / 2.0
                } else {
                    gap_delta
                };
            }
            if leaf.column == 0 {
                contribution += leaf.start_mbp;
            }
            if leaf.column + 1 == plan.track_count {
                contribution += leaf.end_mbp;
            }
            max_content[leaf.column] = max_content[leaf.column].max(contribution.max(0.0));
        }
        let max_sum: f32 = max_content.iter().sum();
        if !valid || max_sum > target_track_sum + 0.01 {
            continue;
        }
        let stretch = (target_track_sum - max_sum) / plan.track_count as f32;
        let used: Vec<f32> = max_content.iter().map(|width| width + stretch).collect();
        if used.iter().any(|width| !width.is_finite() || *width < 0.0) {
            continue;
        }

        // Validate the entire copied chain before mutating anything. A large
        // edge MBP or gap delta can exhaust an outer track; declining the plan
        // atomically is safer than leaving only the ancestor frozen.
        let mut copied_wrappers = Vec::with_capacity(plan.wrappers.len());
        for wrapper in &plan.wrappers {
            let mut copied = used.clone();
            if plan.track_count > 1 {
                let root_half = plan.parent_gap / 2.0;
                let child_half = wrapper.gap / 2.0;
                for (index, width) in copied.iter_mut().enumerate() {
                    *width += if index == 0 || index + 1 == plan.track_count {
                        root_half - child_half
                    } else {
                        plan.parent_gap - wrapper.gap
                    };
                }
            }
            copied[0] -= wrapper.start_mbp;
            copied[plan.track_count - 1] -= wrapper.end_mbp;
            if copied
                .iter()
                .any(|width| !width.is_finite() || *width < 0.0)
                || taffy_tree.style(wrapper.node).is_err()
            {
                valid = false;
                break;
            }
            copied_wrappers.push((wrapper.node, copied));
        }
        if !valid || taffy_tree.style(plan.parent).is_err() {
            continue;
        }

        let mut parent_style = taffy_tree.style(plan.parent).unwrap().clone();
        parent_style.grid_template_columns = fixed_grid_tracks(&used);
        let _ = taffy_tree.set_style(plan.parent, parent_style);
        for (node, copied) in copied_wrappers {
            let mut style = taffy_tree.style(node).unwrap().clone();
            style.grid_template_columns = fixed_grid_tracks(&copied);
            let _ = taffy_tree.set_style(node, style);
        }
        changed = true;
    }
    changed
}

/// Resolve a raw `grid-column`/`grid-row` value that names grid lines into a
/// numeric `taffy::Line`, using `map` (line-name -> 1-based line number). Handles
/// `a / b` (each side a name, integer, or `span n`) and the single-ident
/// `grid-column: foo` area shorthand (`foo-start / foo-end`). Returns `None` when
/// a referenced name is absent, leaving the item to auto-place.
pub(crate) fn resolve_named_placement(
    raw: &str,
    map: &HashMap<String, i16>,
) -> Option<taffy::Line<taffy::GridPlacement>> {
    use taffy::style_helpers::{line, span};
    let side = |tok: &str, is_start: bool| -> Option<taffy::GridPlacement> {
        let t = tok.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("auto") {
            return Some(taffy::GridPlacement::Auto);
        }
        if let Some(n) = t.strip_prefix("span") {
            if let Ok(s) = n.trim().parse::<u16>() {
                return Some(span(s));
            }
        }
        if let Ok(i) = t.parse::<i16>() {
            return Some(line(i));
        }
        map.get(t)
            .or_else(|| map.get(&format!("{t}-{}", if is_start { "start" } else { "end" })))
            .map(|&l| line(l))
    };
    if let Some((a, b)) = raw.split_once('/') {
        Some(taffy::Line {
            start: side(a, true)?,
            end: side(b, false)?,
        })
    } else {
        let name = raw.trim();
        if let Some(&s) = map.get(&format!("{name}-start")) {
            let end = map
                .get(&format!("{name}-end"))
                .map(|&e| line(e))
                .unwrap_or(taffy::GridPlacement::Auto);
            return Some(taffy::Line {
                start: line(s),
                end,
            });
        }
        map.get(name).map(|&s| taffy::Line {
            start: line(s),
            end: taffy::GridPlacement::Auto,
        })
    }
}

pub(crate) fn compute_absolute_rects(
    taffy_tree: &TaffyTree<usize>,
    taffy_id: taffy::NodeId,
    abs_x: f32,
    abs_y: f32,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    words: &HashMap<taffy::NodeId, (NodeId, String)>,
    rects: &mut HashMap<NodeId, Rect>,
    text_runs: &mut HashMap<NodeId, Vec<(Rect, String)>>,
    anon_rects: &mut HashMap<usize, Rect>,
    generated_nodes: &HashMap<taffy::NodeId, usize>,
    generated_rects: &mut [Option<Rect>],
) {
    if let Ok(layout) = taffy_tree.layout(taffy_id) {
        let x = abs_x + layout.location.x;
        let y = abs_y + layout.location.y;
        let rect = Rect {
            x,
            y,
            width: layout.size.width,
            height: layout.size.height,
        };

        if let Some(dom_id) = id_map.get(&taffy_id) {
            rects.insert(*dom_id, rect);
        } else if let Some(&item) = taffy_tree.get_node_context(taffy_id) {
            // A taffy leaf with an engine-item context but no DOM id is an
            // anonymous inline-run leaf (see `build_mixed_block`); record its
            // final rect by item index so the finalize pass can pin it.
            anon_rects.insert(item, rect);
        }
        if let Some(index) = generated_nodes.get(&taffy_id) {
            generated_rects[*index] = Some(rect);
        }
        // A word leaf's dom_id is its owning text node, shared by every other
        // word from the same node, so this appends rather than overwrites.
        if let Some((text_dom_id, word)) = words.get(&taffy_id) {
            text_runs
                .entry(*text_dom_id)
                .or_default()
                .push((rect, word.clone()));
        }

        if let Ok(children) = taffy_tree.children(taffy_id) {
            for child_id in children {
                compute_absolute_rects(
                    taffy_tree,
                    child_id,
                    x,
                    y,
                    id_map,
                    words,
                    rects,
                    text_runs,
                    anon_rects,
                    generated_nodes,
                    generated_rects,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct StaticPositionCandidate {
    pub(crate) child: taffy::NodeId,
    pub(crate) target: taffy::NodeId,
    pub(crate) inline_axis: bool,
    pub(crate) block_axis: bool,
}

/// Attach positioned boxes to their CSS containing block rather than their
/// immediate DOM parent.
///
/// Taffy resolves an absolute child's insets against its direct layout-tree
/// parent. CSS instead uses the nearest positioned or transformed ancestor;
/// fixed boxes use the nearest transformed ancestor or the initial containing
/// block. A box with a fully-auto axis first remains in its original formatting
/// context so taffy can produce the placeholder-like static coordinate. The
/// caller harvests that coordinate and reparents it in a bounded second pass.
pub(crate) fn reparent_inset_positioned_nodes(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    taffy_root: taffy::NodeId,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<StaticPositionCandidate> {
    let reverse: HashMap<NodeId, taffy::NodeId> = id_map
        .iter()
        .map(|(&taffy_id, &dom_id)| (dom_id, taffy_id))
        .collect();
    let mut nearest_abs_cb_for_children: HashMap<NodeId, taffy::NodeId> = HashMap::new();
    let mut nearest_fixed_cb_for_children: HashMap<NodeId, taffy::NodeId> = HashMap::new();
    let mut static_candidates = Vec::new();

    for dom_id in rendered_descendants(tree, tree.document()) {
        let Some(style) = styles.get(&dom_id) else {
            continue;
        };
        let parent = rendered_parent(tree, dom_id);
        let inherited_abs_cb = parent
            .and_then(|id| nearest_abs_cb_for_children.get(&id).copied())
            .unwrap_or(taffy_root);
        let inherited_fixed_cb = parent
            .and_then(|id| nearest_fixed_cb_for_children.get(&id).copied())
            .unwrap_or(taffy_root);

        // Record this before any candidate early-exit so all descendants get
        // O(1) nearest-containing-block lookups. Positioned and transformed
        // boxes capture absolute descendants; only transformed boxes capture
        // fixed descendants. The full walk stays O(n).
        let own_box = reverse.get(&dom_id).copied();
        let establishes_cb = style.establishes_positioning_containing_block();
        let abs_child_cb = if style.position.is_some() || establishes_cb {
            own_box.unwrap_or(inherited_abs_cb)
        } else {
            inherited_abs_cb
        };
        let fixed_child_cb = if establishes_cb {
            own_box.unwrap_or(inherited_fixed_cb)
        } else {
            inherited_fixed_cb
        };
        nearest_abs_cb_for_children.insert(dom_id, abs_child_cb);
        nearest_fixed_cb_for_children.insert(dom_id, fixed_child_cb);

        if !matches!(style.position, Some(taffy::Position::Absolute)) {
            continue;
        }
        let has_block_inset = style.inset[0].is_some() || style.inset[2].is_some();
        let has_inline_inset = style.inset[1].is_some() || style.inset[3].is_some();
        let Some(&child) = reverse.get(&dom_id) else {
            continue;
        };
        let target = if style.position_fixed {
            inherited_fixed_cb
        } else {
            inherited_abs_cb
        };
        let Some(current) = taffy_tree.parent(child) else {
            continue;
        };
        if current == target {
            continue;
        }
        if !has_block_inset || !has_inline_inset {
            static_candidates.push(StaticPositionCandidate {
                child,
                target,
                inline_axis: !has_inline_inset,
                block_axis: !has_block_inset,
            });
            continue;
        }
        if taffy_tree.remove_child(current, child).is_ok() {
            let _ = taffy_tree.add_child(target, child);
        }
    }
    static_candidates
}

pub(crate) fn taffy_global_origin(
    taffy_tree: &TaffyTree<usize>,
    node: taffy::NodeId,
) -> Option<(f32, f32)> {
    let mut current = Some(node);
    let mut x = 0.0;
    let mut y = 0.0;
    while let Some(id) = current {
        let layout = taffy_tree.layout(id).ok()?;
        x += layout.location.x;
        y += layout.location.y;
        current = taffy_tree.parent(id);
    }
    Some((x, y))
}

fn collect_taffy_global_rects(
    taffy_tree: &TaffyTree<usize>,
    node: taffy::NodeId,
    parent_x: f32,
    parent_y: f32,
    rects: &mut HashMap<taffy::NodeId, Rect>,
) {
    let Ok(layout) = taffy_tree.layout(node) else {
        return;
    };
    let x = parent_x + layout.location.x;
    let y = parent_y + layout.location.y;
    rects.insert(
        node,
        Rect {
            x,
            y,
            width: layout.size.width,
            height: layout.size.height,
        },
    );
    for child in taffy_tree.children(node).unwrap_or_default() {
        collect_taffy_global_rects(taffy_tree, child, x, y, rects);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FloatBand{
    top: f32,
    bottom: f32,
    left: f32,
    right: f32,
    side: crate::Float,
}

pub(crate) fn narrow_node_to_float_band(
    taffy_tree: &mut TaffyTree<usize>,
    node: taffy::NodeId,
    preliminary_rects: &HashMap<taffy::NodeId, Rect>,
    band: FloatBand,
) -> bool {
    let Some(&rect) = preliminary_rects.get(&node) else {
        return false;
    };
    if rect.y >= band.bottom || rect.y + rect.height <= band.top || rect.width <= 0.0 {
        return false;
    }
    let Ok(current) = taffy_tree.style(node) else {
        return false;
    };
    let Ok(layout) = taffy_tree.layout(node) else {
        return false;
    };
    let mut narrowed = current.clone();
    let (available, left_shift) = match band.side {
        crate::Float::Right => {
            if band.left >= rect.x + rect.width {
                return false;
            }
            ((band.left - rect.x).max(0.0), 0.0)
        }
        crate::Float::Left => {
            if band.right <= rect.x {
                return false;
            }
            let shift = (band.right - rect.x).max(0.0);
            ((rect.width - shift).max(0.0), shift)
        }
    };
    if available >= rect.width - 0.01 {
        return false;
    }
    let specified = if current.box_sizing == taffy::BoxSizing::ContentBox {
        (available
            - layout.padding.left
            - layout.padding.right
            - layout.border.left
            - layout.border.right)
            .max(0.0)
    } else {
        available
    };
    narrowed.size.width = taffy::Dimension::length(specified);
    narrowed.max_size.width = taffy::Dimension::length(specified);
    if left_shift > 0.0 {
        narrowed.margin.left = taffy::LengthPercentageAuto::length(layout.margin.left + left_shift);
    }
    taffy_tree.set_style(node, narrowed).is_ok()
}

pub(crate) fn grow_bfc_to_float_bottom(
    taffy_tree: &mut TaffyTree<usize>,
    node: taffy::NodeId,
    preliminary_rects: &HashMap<taffy::NodeId, Rect>,
    float_bottom: f32,
) -> bool {
    let Some(&rect) = preliminary_rects.get(&node) else {
        return false;
    };
    let desired_border_height = (float_bottom - rect.y).max(0.0);
    if desired_border_height <= rect.height + 0.01 {
        return false;
    }
    let Ok(current) = taffy_tree.style(node) else {
        return false;
    };
    let Ok(layout) = taffy_tree.layout(node) else {
        return false;
    };
    let specified = if current.box_sizing == taffy::BoxSizing::ContentBox {
        (desired_border_height
            - layout.padding.top
            - layout.padding.bottom
            - layout.border.top
            - layout.border.bottom)
            .max(0.0)
    } else {
        desired_border_height
    };
    let mut grown = current.clone();
    grown.min_size.height = taffy::Dimension::length(specified);
    taffy_tree.set_style(node, grown).is_ok()
}

pub(crate) fn narrow_intersecting_descendants(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    reverse: &HashMap<NodeId, taffy::NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    ifc_items: &IfcRegistry,
    preliminary_rects: &HashMap<taffy::NodeId, Rect>,
    band: FloatBand,
) -> bool {
    let Some(&node) = reverse.get(&id) else {
        return false;
    };
    let Some(&rect) = preliminary_rects.get(&node) else {
        return false;
    };
    if rect.y >= band.bottom || rect.y + rect.height <= band.top {
        return false;
    }
    let Some(style) = styles.get(&id) else {
        return false;
    };
    if style.display == crate::Display::None
        || style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
    {
        return false;
    }

    // Float-avoiding formatting contexts move as one box. A shaped IFC or a
    // leaf has no deeper block boundary at which its width can change, so it
    // also consumes the current band as a unit.
    let in_flow_element_children: Vec<NodeId> = tree
        .children(id)
        .into_iter()
        .filter(|child| {
            styles.get(child).map_or(false, |child_style| {
                child_style.display != crate::Display::None
                    && child_style.float.is_none()
                    && !matches!(child_style.position, Some(taffy::Position::Absolute))
            })
        })
        .collect();
    if establishes_block_formatting_context(style)
        || ifc_items.whole.contains_key(&id)
        || in_flow_element_children.is_empty()
    {
        return narrow_node_to_float_band(taffy_tree, node, preliminary_rects, band);
    }

    let mut changed = false;
    for child in in_flow_element_children {
        changed |= narrow_intersecting_descendants(
            tree,
            child,
            taffy_tree,
            reverse,
            styles,
            ifc_items,
            preliminary_rects,
            band,
        );
    }
    changed
}

pub(crate) fn apply_float_continuations(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    ifc_items: &IfcRegistry,
) -> bool {
    let reverse: HashMap<NodeId, taffy::NodeId> =
        id_map.iter().map(|(&taffy, &dom)| (dom, taffy)).collect();
    let Some(root) = id_map
        .keys()
        .copied()
        .find(|node| taffy_tree.parent(*node).is_none())
    else {
        return false;
    };
    let mut preliminary_rects = HashMap::with_capacity(id_map.len());
    collect_taffy_global_rects(taffy_tree, root, 0.0, 0.0, &mut preliminary_rects);
    let mut changed = false;
    for continuation in &ifc_items.float_continuations {
        let Some(&float_rect) = preliminary_rects.get(&continuation.float) else {
            continue;
        };
        let Ok(float_layout) = taffy_tree.layout(continuation.float) else {
            continue;
        };
        let Some(&flow_rect) = preliminary_rects.get(&continuation.flow) else {
            continue;
        };
        let band = FloatBand {
            top: float_rect.y - float_layout.margin.top,
            bottom: float_rect.y + float_rect.height + float_layout.margin.bottom,
            left: float_rect.x - float_layout.margin.left,
            right: float_rect.x + float_rect.width + float_layout.margin.right,
            side: continuation.side,
        };
        if band.bottom <= flow_rect.y + flow_rect.height + 0.01 {
            continue;
        }

        // A non-BFC wrapper is transparent to the BFC's float manager. Visit
        // the wrapper's following siblings, then repeat at each ancestor until
        // (and including) the nearest ancestor BFC. Descend through ordinary
        // blocks so only the leaf/block bands that actually intersect the
        // float are narrowed; later siblings below the float stay full width.
        let mut current = continuation.owner;
        while let Some(parent) = rendered_parent(tree, current) {
            let siblings = rendered_children(tree, parent);
            let Some(index) = siblings.iter().position(|candidate| *candidate == current) else {
                break;
            };
            for sibling in &siblings[index + 1..] {
                changed |= narrow_intersecting_descendants(
                    tree,
                    *sibling,
                    taffy_tree,
                    &reverse,
                    styles,
                    ifc_items,
                    &preliminary_rects,
                    band,
                );
            }
            let reached_bfc = styles
                .get(&parent)
                .map(establishes_block_formatting_context)
                .unwrap_or(false);
            if reached_bfc {
                if styles
                    .get(&parent)
                    .map(|style| matches!(style.height, crate::Dimension::Auto))
                    .unwrap_or(false)
                {
                    if let Some(&bfc_node) = reverse.get(&parent) {
                        changed |= grow_bfc_to_float_bottom(
                            taffy_tree,
                            bfc_node,
                            &preliminary_rects,
                            band.bottom,
                        );
                    }
                }
                break;
            }
            current = parent;
        }
    }
    changed
}
