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
use crate::dom_grid::{apply_float_continuations, apply_full_span_column_subgrids, collect_effective_grid_children, compute_absolute_rects, effective_grid_child_style, reparent_inset_positioned_nodes, resolve_grid_areas, taffy_global_origin, EffectiveGridChild, StaticPositionCandidate};
use crate::dom_sticky::{DerivedGeometryState, DerivedLayoutState, StickyFrame, StickyLayout};
use crate::{to_taffy_style, Rect};

use super::*;
use crate::dom::{RetainedStyleMaps, container_snapshot, layout_dom_once, rendered_descendants, rendered_parent};

pub fn layout_dom(tree: &DomTree, viewport: (f32, f32)) -> DomLayout {
    layout_dom_with_images(tree, viewport, &HashMap::new())
}

/// Like [`layout_dom`], but `intrinsic` supplies fetched intrinsic pixel sizes
/// (width, height) for replaced elements keyed by `NodeId`. Paint collects
/// these before layout (it has the base URL and the image cache) so a
/// CSS-sized `<img>` with no width/height attribute gets a real box from its
/// intrinsic ratio instead of collapsing to zero area.
pub fn layout_dom_with_images(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &HashMap<NodeId, (f32, f32)>,
) -> DomLayout {
    layout_dom_with_resources(tree, viewport, intrinsic, &[])
}

/// Like [`layout_dom_with_images`], with decoded OpenType web-font data loaded
/// into the shaping database for this render pass.
pub fn layout_dom_with_resources(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &HashMap<NodeId, (f32, f32)>,
    fonts: &[Vec<u8>],
) -> DomLayout {
    let fonts: Vec<_> = fonts
        .iter()
        .map(|data| crate::inline::WebFont {
            data: data.clone(),
            family: None,
            weight: None,
            italic: None,
        })
        .collect();
    let intrinsic = intrinsic
        .iter()
        .filter_map(|(&nid, &(width, height))| {
            (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
                .then(|| (nid, crate::ReplacedIntrinsic::from_dimensions(width, height)))
        })
        .collect();
    layout_dom_with_web_fonts(tree, viewport, &intrinsic, &fonts)
}

pub(crate) type ReplacedIntrinsicMap = HashMap<NodeId, crate::ReplacedIntrinsic>;

pub(crate) const CONTAINER_LAYOUT_SAFETY_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContainerLayoutTermination {
    NoQueries,
    NoContainers,
    GeometryStable,
    SignatureStable,
    OscillationFallback,
    PassCapFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContainerLayoutTelemetry {
    pub(crate) passes: usize,
    pub(crate) termination: ContainerLayoutTermination,
    pub(crate) query: crate::css::ContainerQueryStats,
    pub(crate) retained_reused: usize,
    pub(crate) retained_fresh: usize,
    pub(crate) retained_fallback: usize,
}

pub(crate) fn container_iteration_termination<T: PartialEq>(
    geometry_stable: bool,
    signature: &T,
    previous_signature: Option<&T>,
) -> Option<ContainerLayoutTermination> {
    if geometry_stable {
        Some(ContainerLayoutTermination::GeometryStable)
    } else if previous_signature == Some(signature) {
        Some(ContainerLayoutTermination::SignatureStable)
    } else {
        None
    }
}

pub(crate) fn layout_dom_with_web_fonts(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
) -> DomLayout {
    layout_dom_with_web_fonts_measured(tree, viewport, intrinsic, fonts).0
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
) -> DomLayout {
    layout_dom_with_web_fonts_and_stylesheet_cache_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        crate::AnimationSampleTime::default(),
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    animation_sample_time: crate::AnimationSampleTime,
) -> DomLayout {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    layout_dom_with_web_fonts_and_stylesheet_cache_with_animation_state(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        crate::AnimationSample {
            time: animation_sample_time,
            mode: crate::AnimationSampleMode::DocumentTime,
        },
        &mut animation_timeline,
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> DomLayout {
    layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        crate::CssMediaType::Screen,
        animation_sample,
        animation_timeline,
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    media_type: crate::CssMediaType,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> DomLayout {
    layout_dom_with_web_fonts_pass_limit_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        None,
        Some(stylesheet_cache),
        None,
        &[],
        media_type,
        animation_sample,
        animation_timeline,
    )
    .0
}

#[allow(dead_code)]
pub(crate) fn layout_dom_with_web_fonts_and_retained_styles(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: RetainedStyleMaps,
    mutations: &[RetainedStyleMutation],
) -> DomLayout {
    layout_dom_with_web_fonts_and_retained_styles_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        retained,
        mutations,
        crate::AnimationSampleTime::default(),
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_retained_styles_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: RetainedStyleMaps,
    mutations: &[RetainedStyleMutation],
    animation_sample_time: crate::AnimationSampleTime,
) -> DomLayout {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    layout_dom_with_web_fonts_and_retained_styles_with_animation_state(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        retained,
        mutations,
        crate::AnimationSample {
            time: animation_sample_time,
            mode: crate::AnimationSampleMode::DocumentTime,
        },
        &mut animation_timeline,
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_retained_styles_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: RetainedStyleMaps,
    mutations: &[RetainedStyleMutation],
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> DomLayout {
    layout_dom_with_web_fonts_pass_limit_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        None,
        Some(stylesheet_cache),
        Some(retained),
        mutations,
        crate::CssMediaType::Screen,
        animation_sample,
        animation_timeline,
    )
    .0
}

pub(crate) fn layout_dom_with_web_fonts_measured(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
) -> (DomLayout, ContainerLayoutTelemetry) {
    layout_dom_with_web_fonts_pass_limit(tree, viewport, intrinsic, fonts, None, None, None, &[])
}

pub(crate) fn layout_dom_with_web_fonts_pass_limit(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    pass_limit: Option<usize>,
    stylesheet_cache: Option<&mut crate::css::StylesheetCache>,
    retained: Option<RetainedStyleMaps>,
    mutations: &[RetainedStyleMutation],
) -> (DomLayout, ContainerLayoutTelemetry) {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    layout_dom_with_web_fonts_pass_limit_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        pass_limit,
        stylesheet_cache,
        retained,
        mutations,
        crate::CssMediaType::Screen,
        crate::AnimationSample::default(),
        &mut animation_timeline,
    )
}

/// Compile one author stylesheet per native ShadowRoot.
///
/// `DomTree::descendants` deliberately stays inside one tree scope, so the
/// document collector cannot accidentally absorb a component's styles and a
/// shadow collector cannot absorb a nested component's styles. Walking host
/// edges explicitly here also discovers roots nested inside other roots.
pub(crate) fn collect_shadow_stylesheets(
    tree: &DomTree,
    viewport: (f32, f32),
    media_type: crate::CssMediaType,
) -> HashMap<NodeId, std::sync::Arc<crate::css::Stylesheet>> {
    let mut roots = Vec::new();
    let mut stack = vec![tree.document()];
    let mut visited = HashSet::new();
    while let Some(node) = stack.pop() {
        if !visited.insert(node) {
            continue;
        }
        if let Some(root) = tree.shadow_root(node) {
            roots.push(root);
            stack.extend(tree.children(root).into_iter().rev());
        }
        stack.extend(tree.children(node).into_iter().rev());
    }

    roots
        .into_iter()
        .map(|root| {
            let sources = tree
                .descendants(root)
                .into_iter()
                .filter_map(|node_id| {
                    let node = tree.get_node(node_id)?;
                    let element = node.as_element()?;
                    (element.local.as_ref() == "style"
                        && node.get_attribute("media").is_none_or(|media| {
                            media.trim().is_empty()
                                || crate::css::media_query_applies_for_viewport_and_type(
                                    media,
                                    viewport,
                                    media_type,
                                )
                        }))
                    .then(|| tree.text_content(node_id))
                })
                .collect::<Vec<_>>();
            let sheet = crate::css::Stylesheet::parse_for_viewport_and_media(
                tree,
                &sources,
                viewport,
                media_type,
            );
            (root, std::sync::Arc::new(sheet))
        })
        .collect()
}

pub(crate) fn layout_dom_with_web_fonts_pass_limit_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    pass_limit: Option<usize>,
    stylesheet_cache: Option<&mut crate::css::StylesheetCache>,
    retained: Option<RetainedStyleMaps>,
    mutations: &[RetainedStyleMutation],
    media_type: crate::CssMediaType,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> (DomLayout, ContainerLayoutTelemetry) {
    let timing = std::env::var("OBSCURA_RENDER_TIMING").is_ok();

    // Collect the text of every <style> block in document order.
    let mut css_sources = Vec::new();
    for nid in tree.descendants(tree.document()) {
        if let Some(node) = tree.get_node(nid) {
            if let Some(elem) = node.as_element() {
                if elem.local.as_ref() == "style"
                    && node.get_attribute("media").is_none_or(|media| {
                        media.trim().is_empty()
                            || crate::css::media_query_applies_for_viewport_and_type(
                                media,
                                viewport,
                                media_type,
                            )
                    })
                {
                    css_sources.push(tree.text_content(nid));
                }
            }
        }
    }

    let t0 = std::time::Instant::now();
    let (sheet, stylesheet_cache_hit) = match stylesheet_cache {
        Some(cache) => cache.get_or_parse(tree, &css_sources, viewport, media_type),
        None => (
            std::sync::Arc::new(crate::css::Stylesheet::parse_for_viewport_and_media(
                tree,
                &css_sources,
                viewport,
                media_type,
            )),
            false,
        ),
    };
    let shadow_sheets = collect_shadow_stylesheets(tree, viewport, media_type);
    let t_parse = t0.elapsed();

    let retained_requested = retained.as_ref().map_or(0, |retained| retained.styles.len());
    let retained = retained.and_then(|mut retained| {
        // The document cache key intentionally contains only document-scope
        // sources. Until shadow sheets have their own retained cache keys and
        // invalidation maps, reusing computed styles after DOM/style damage in
        // a document with a native root could preserve stale shadow rules or
        // inherited host custom properties. Resource-only damage cannot
        // change either cascade, so its zero-dirty reuse remains sound.
        let resource_only = !mutations.is_empty()
            && mutations
                .iter()
                .all(|mutation| matches!(mutation, RetainedStyleMutation::Resource));
        if !shadow_sheets.is_empty() && !resource_only {
            return None;
        }
        if !stylesheet_cache_hit {
            return None;
        }
        let active_containers = retained
            .styles
            .iter()
            .filter_map(|(node, style)| {
                (style.container_type != crate::ContainerType::Normal).then_some(*node)
            })
            .collect::<HashSet<_>>();
        let connected = std::iter::once(tree.document())
            .chain(rendered_descendants(tree, tree.document()))
            .collect::<HashSet<_>>();
        retained.styles.retain(|node, _| connected.contains(node));
        retained
            .custom_properties
            .retain(|node, _| connected.contains(node));
        match retained_style_plan(tree, &sheet, mutations) {
            RetainedStylePlan::Reuse {
                mut dirty,
                has_animation_damage,
            } => {
                if !active_containers.is_empty() && sheet.has_container_queries() {
                    let mut matcher = tree.matcher();
                    add_container_query_reset_scopes(
                        tree,
                        tree.document(),
                        &sheet,
                        &mut matcher,
                        &active_containers,
                        false,
                        false,
                        &mut dirty,
                    );
                }
                // Once animation damage reaches at least half of the retained
                // style graph, sparse HashMap reuse no longer offsets dirty-set
                // bookkeeping and branch checks. This is a document-relative
                // coverage threshold, not a site- or node-count heuristic.
                if has_animation_damage
                    && dirty.len().saturating_mul(2) >= retained.styles.len()
                {
                    if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                        eprintln!(
                            "[timing] retained-style fallback reason=animation-dirty-coverage dirty={} retained={}",
                            dirty.len(),
                            retained.styles.len(),
                        );
                    }
                    return None;
                }
                Some((retained, dirty))
            }
            RetainedStylePlan::Full => None,
        }
    });
    let retained_fresh = retained.as_ref().map_or(0, |(retained, fresh)| {
        retained
            .styles
            .keys()
            .filter(|node| fresh.contains(node))
            .count()
    });
    let retained_reused = retained
        .as_ref()
        .map_or(0, |(retained, _)| retained.styles.len() - retained_fresh);
    let retained_fallback = usize::from(retained_requested != 0 && retained.is_none());
    let (mut laid, _, mut query, mut cascade_time) =
        layout_dom_once(
            tree,
            viewport,
            intrinsic,
            fonts,
            &sheet,
            &shadow_sheets,
            None,
            retained,
            animation_sample,
            animation_timeline,
        );
    if !sheet.has_container_queries() {
        if timing {
            let (r, i, c, a, l, u) = sheet.debug_stats();
            eprintln!("[timing] parse+index={:?} stylesheet_cache_hit={} cascade={:?} rules={} id_keys={} class_keys={} attr_keys={} local_keys={} universal={} cq_passes=1 cq_termination=no-queries retained_reused={} retained_fresh={} retained_fallback={}", t_parse, stylesheet_cache_hit, cascade_time, r, i, c, a, l, u, retained_reused, retained_fresh, retained_fallback);
        }
        return (
            laid,
            ContainerLayoutTelemetry {
                passes: 1,
                termination: ContainerLayoutTermination::NoQueries,
                query,
                retained_reused,
                retained_fresh,
                retained_fallback,
            },
        );
    }

    let mut snapshot = container_snapshot(tree, &laid);
    // A container condition has no matching query container when the initial
    // cascade produced neither container-type nor container-name. Re-running
    // the entire cascade cannot create the first container because conditional
    // rules are inactive until a container already exists. Large framework
    // stylesheets commonly ship dormant @container blocks; keeping them on the
    // one-pass path avoids a redundant whole-document layout.
    if snapshot.boxes.is_empty() {
        if timing {
            let (r, i, c, a, l, u) = sheet.debug_stats();
            eprintln!("[timing] parse+index={:?} stylesheet_cache_hit={} cascade={:?} rules={} id_keys={} class_keys={} attr_keys={} local_keys={} universal={} cq_passes=1 cq_termination=no-containers retained_reused={} retained_fresh={} retained_fallback={}", t_parse, stylesheet_cache_hit, cascade_time, r, i, c, a, l, u, retained_reused, retained_fresh, retained_fallback);
        }
        return (
            laid,
            ContainerLayoutTelemetry {
                passes: 1,
                termination: ContainerLayoutTermination::NoContainers,
                query,
                retained_reused,
                retained_fresh,
                retained_fallback,
            },
        );
    }
    let mut previous_candidate: Option<(DomLayout, crate::css::ContainerDecisionSignature)> = None;
    let mut seen_signatures = Vec::new();
    let mut passes = 1;
    let mut termination = ContainerLayoutTermination::PassCapFallback;
    // Gecko permits at most one CQ-triggered update per container element in
    // one flush and processes ancestors before descendants. Our whole-tree
    // passes need the same order of growth: a chain can legitimately reveal
    // one deeper query container per pass. Scale the useful bound with DOM
    // ancestry and nested conditional depth, retaining a high safety limit
    // against adversarial non-convergence. Hitting it is visible telemetry and
    // uses the conservative fallback below, never a silently stale layout.
    // `descendants` is preorder, so every parent depth is available before
    // its children. Keep this O(nodes): walking every ancestor separately
    // makes a deeply nested document quadratic before layout even starts.
    let mut element_depths = HashMap::new();
    element_depths.insert(tree.document(), 0usize);
    let mut max_dom_depth = 1usize;
    for id in rendered_descendants(tree, tree.document()) {
        let Some(node) = tree.get_node(id) else {
            continue;
        };
        let parent_depth = rendered_parent(tree, id)
            .and_then(|parent| element_depths.get(&parent).copied())
            .unwrap_or(0);
        let depth = parent_depth + usize::from(node.is_element());
        element_depths.insert(id, depth);
        max_dom_depth = max_dom_depth.max(depth);
    }
    let max_passes = pass_limit.unwrap_or_else(|| {
        (max_dom_depth + sheet.container_condition_depth() + 2)
            .clamp(4, CONTAINER_LAYOUT_SAFETY_LIMIT)
    });
    let mut needs_fallback = false;
    for pass in 2..=max_passes {
        let (next, signature, pass_query, pass_cascade) =
            layout_dom_once(
                tree,
                viewport,
                intrinsic,
                fonts,
                &sheet,
                &shadow_sheets,
                Some(&snapshot),
                None,
                animation_sample,
                animation_timeline,
            );
        passes = pass;
        query.evaluations += pass_query.evaluations;
        query.cache_hits += pass_query.cache_hits;
        query.ancestor_steps += pass_query.ancestor_steps;
        cascade_time += pass_cascade;
        let next_snapshot = container_snapshot(tree, &next);
        let signature = signature.expect("container pass must produce a signature");
        if let Some(reason) = container_iteration_termination(
            next_snapshot == snapshot,
            &signature,
            previous_candidate.as_ref().map(|(_, signature)| signature),
        ) {
            termination = reason;
            // Equal adjacent signatures prove that the *previous* candidate's
            // applied decisions match an evaluation of its own final
            // snapshot. Returning `next` here would be off by one and could
            // expose styles evaluated against geometry it no longer has.
            laid = if reason == ContainerLayoutTermination::SignatureStable {
                previous_candidate
                    .take()
                    .expect("stable signature requires a previous candidate")
                    .0
            } else {
                next
            };
            break;
        }
        if seen_signatures.contains(&signature) {
            termination = ContainerLayoutTermination::OscillationFallback;
            needs_fallback = true;
            break;
        }
        seen_signatures.push(signature.clone());
        previous_candidate = Some((next, signature));
        snapshot = next_snapshot;
        if pass == max_passes {
            needs_fallback = true;
        }
    }

    if needs_fallback {
        // Author-controlled CSS must never crash rendering, but neither may
        // we return a layout whose conditional declarations contradict the
        // geometry used to choose them. Disable the unstable conditional
        // rules for this render and expose the downgrade in telemetry.
        let (fallback, _, fallback_query, fallback_cascade) =
            layout_dom_once(
                tree,
                viewport,
                intrinsic,
                fonts,
                &sheet,
                &shadow_sheets,
                None,
                None,
                animation_sample,
                animation_timeline,
            );
        laid = fallback;
        passes += 1;
        query.evaluations += fallback_query.evaluations;
        query.cache_hits += fallback_query.cache_hits;
        query.ancestor_steps += fallback_query.ancestor_steps;
        cascade_time += fallback_cascade;
    }

    if timing {
        let (r, i, c, a, l, u) = sheet.debug_stats();
        eprintln!("[timing] parse+index={:?} stylesheet_cache_hit={} cascade_total={:?} rules={} id_keys={} class_keys={} attr_keys={} local_keys={} universal={} cq_passes={} cq_termination={:?} cq_evaluations={} cq_cache_hits={} cq_ancestor_steps={} retained_reused={} retained_fresh={} retained_fallback={}", t_parse, stylesheet_cache_hit, cascade_time, r, i, c, a, l, u, passes, termination, query.evaluations, query.cache_hits, query.ancestor_steps, retained_reused, retained_fresh, retained_fallback);
    }
    (
        laid,
        ContainerLayoutTelemetry {
            passes,
            termination,
            query,
            retained_reused,
            retained_fresh,
            retained_fallback,
        },
    )
}

