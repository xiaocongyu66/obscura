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
use crate::{to_taffy_style, Rect};

use super::*;
use crate::dom::{ScrollId, ScrollTree};

/// Prepared geometry shared by CSSOM, scrolling, sticky positioning, and
/// paint. Building these values together avoids independently rediscovering
/// viewport-fixed ownership and root overflow several times per frame.
pub(crate) struct DerivedLayoutState {
    pub content_size: (f32, f32),
    pub viewport_fixed: HashSet<NodeId>,
    pub sticky: StickyLayout,
    pub scroll_tree: ScrollTree,
}

pub(crate) struct DerivedGeometryState {
    pub content_size: (f32, f32),
    pub sticky: StickyLayout,
    pub scroll_tree: ScrollTree,
}

/// Root-scroll sticky-position constraints captured from normal-flow layout.
///
/// The normal boxes stay immutable in the layout cache. A scroll offset is
/// resolved into one accumulated translation per affected node, which keeps
/// JS geometry and screenshot paint on the same path.
#[derive(Debug, Clone, Default)]
pub struct StickyLayout {
    pub(crate) frames: Vec<StickyFrame>,
    pub(crate) owners: HashMap<NodeId, NodeId>,
    pub(crate) clip_owners: HashMap<NodeId, NodeId>,
    // Only populated by the public compatibility constructor. Production
    // prepared renders own the topology separately and avoid duplicating its
    // dense node vectors.
    pub(crate) compatibility_scroll_tree: Option<ScrollTree>,
}

#[derive(Debug, Clone)]
pub(crate) struct StickyFrame {
    pub(crate) id: NodeId,
    pub(crate) parent_sticky: Option<NodeId>,
    pub(crate) scroll_owner: ScrollId,
    pub(crate) scrollport_node: Option<NodeId>,
    pub(crate) scrollport: Rect,
    pub(crate) normal: Rect,
    pub(crate) containing: Rect,
    pub(crate) containing_is_scrollport: bool,
    pub(crate) margin: crate::Edges,
    pub(crate) inset: [Option<crate::Dimension>; 4],
    pub(crate) inset_expressions: [Option<String>; 4],
    pub(crate) font_size: f32,
    pub(crate) root_font_size: f32,
    pub(crate) rtl_inline: bool,
}

impl StickyLayout {
    pub(crate) fn frame_offsets(
        &self,
        viewport: (f32, f32),
        scroll: (f32, f32),
    ) -> HashMap<NodeId, (f32, f32)> {
        if let Some(scroll_tree) = &self.compatibility_scroll_tree {
            let cumulative = root_only_cumulative_scroll(scroll_tree, scroll);
            return self.resolved_frame_offsets(viewport, scroll_tree, &cumulative);
        }
        let mut frame_offsets = HashMap::with_capacity(self.frames.len());
        for frame in &self.frames {
            let inherited = frame
                .parent_sticky
                .and_then(|id| frame_offsets.get(&id).copied())
                .unwrap_or((0.0, 0.0));
            if frame.scroll_owner != ScrollId::ROOT {
                // Tuple-only callers carry no element scroll state. A nested
                // sticky frame therefore has no local movement, but it still
                // inherits an outer root-sticky frame when one exists.
                frame_offsets.insert(frame.id, inherited);
                continue;
            }
            let normal = Rect {
                x: frame.normal.x + inherited.0,
                y: frame.normal.y + inherited.1,
                ..frame.normal
            };
            let containing = Rect {
                x: frame.containing.x + inherited.0,
                y: frame.containing.y + inherited.1,
                ..frame.containing
            };
            let x = sticky_axis_position(
                normal.x,
                normal.width,
                containing.x + frame.margin.left,
                containing.x + containing.width - frame.margin.right - normal.width,
                scroll.0,
                viewport.0,
                resolve_frame_sticky_inset(frame, 3, viewport.0, viewport),
                resolve_frame_sticky_inset(frame, 1, viewport.0, viewport),
                frame.rtl_inline,
            );
            let y = sticky_axis_position(
                normal.y,
                normal.height,
                containing.y + frame.margin.top,
                containing.y + containing.height - frame.margin.bottom - normal.height,
                scroll.1,
                viewport.1,
                resolve_frame_sticky_inset(frame, 0, viewport.1, viewport),
                resolve_frame_sticky_inset(frame, 2, viewport.1, viewport),
                false,
            );
            frame_offsets.insert(
                frame.id,
                (inherited.0 + x - normal.x, inherited.1 + y - normal.y),
            );
        }
        frame_offsets
    }

    pub(crate) fn resolved_translations(
        &self,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        cumulative_scroll: &[(f32, f32)],
    ) -> HashMap<NodeId, (f32, f32)> {
        let frame_offsets = self.resolved_frame_offsets(viewport, scroll_tree, cumulative_scroll);
        self.owners
            .iter()
            .filter_map(|(&id, owner)| frame_offsets.get(owner).copied().map(|offset| (id, offset)))
            .collect()
    }

    pub(crate) fn resolved_root_translations(
        &self,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        scroll: (f32, f32),
    ) -> HashMap<NodeId, (f32, f32)> {
        let cumulative = root_only_cumulative_scroll(scroll_tree, scroll);
        self.resolved_translations(viewport, scroll_tree, &cumulative)
    }

    pub(crate) fn resolved_frame_offsets(
        &self,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        cumulative_scroll: &[(f32, f32)],
    ) -> HashMap<NodeId, (f32, f32)> {
        let mut frame_offsets = HashMap::with_capacity(self.frames.len());
        for frame in &self.frames {
            let inherited = frame
                .parent_sticky
                .and_then(|id| frame_offsets.get(&id).copied())
                .unwrap_or((0.0, 0.0));
            let container = scroll_tree.containers[frame.scroll_owner.index()];
            let content_move = cumulative_scroll
                .get(frame.scroll_owner.index())
                .copied()
                .unwrap_or((0.0, 0.0));
            let port_parent_move = container
                .parent
                .and_then(|parent| cumulative_scroll.get(parent.index()).copied())
                .unwrap_or((0.0, 0.0));
            let port_sticky = frame
                .scrollport_node
                .and_then(|node| self.owners.get(&node))
                .and_then(|owner| frame_offsets.get(owner).copied())
                .unwrap_or((0.0, 0.0));
            let normal = Rect {
                x: frame.normal.x + content_move.0 + inherited.0,
                y: frame.normal.y + content_move.1 + inherited.1,
                ..frame.normal
            };
            let scrollport = if frame.scroll_owner == ScrollId::ROOT {
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: viewport.0,
                    height: viewport.1,
                }
            } else {
                Rect {
                    x: frame.scrollport.x + port_parent_move.0 + port_sticky.0,
                    y: frame.scrollport.y + port_parent_move.1 + port_sticky.1,
                    ..frame.scrollport
                }
            };
            let containing_move = if frame.containing_is_scrollport {
                (
                    port_parent_move.0 + port_sticky.0,
                    port_parent_move.1 + port_sticky.1,
                )
            } else {
                (
                    content_move.0 + inherited.0,
                    content_move.1 + inherited.1,
                )
            };
            let containing = Rect {
                x: frame.containing.x + containing_move.0,
                y: frame.containing.y + containing_move.1,
                ..frame.containing
            };
            let x = sticky_axis_position(
                normal.x,
                normal.width,
                containing.x + frame.margin.left,
                containing.x + containing.width - frame.margin.right - normal.width,
                scrollport.x,
                scrollport.width,
                resolve_frame_sticky_inset(frame, 3, scrollport.width, viewport),
                resolve_frame_sticky_inset(frame, 1, scrollport.width, viewport),
                frame.rtl_inline,
            );
            let y = sticky_axis_position(
                normal.y,
                normal.height,
                containing.y + frame.margin.top,
                containing.y + containing.height - frame.margin.bottom - normal.height,
                scrollport.y,
                scrollport.height,
                resolve_frame_sticky_inset(frame, 0, scrollport.height, viewport),
                resolve_frame_sticky_inset(frame, 2, scrollport.height, viewport),
                false,
            );
            frame_offsets.insert(
                frame.id,
                (
                    inherited.0 + x - normal.x,
                    inherited.1 + y - normal.y,
                ),
            );
        }
        frame_offsets
    }

    pub(crate) fn resolved_translation_for(
        &self,
        id: NodeId,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        cumulative_scroll: &[(f32, f32)],
    ) -> (f32, f32) {
        let Some(owner) = self.owners.get(&id) else {
            return (0.0, 0.0);
        };
        self.resolved_frame_offsets(viewport, scroll_tree, cumulative_scroll)
            .get(owner)
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// Resolve a single geometry query without materializing an entry for
    /// every descendant in every sticky subtree. This is O(sticky frames), not
    /// O(DOM), and is the hot path for repeated getBoundingClientRect reads.
    pub fn translation_for(
        &self,
        id: NodeId,
        viewport: (f32, f32),
        scroll: (f32, f32),
    ) -> (f32, f32) {
        let Some(owner) = self.owners.get(&id) else {
            return (0.0, 0.0);
        };
        self.frame_offsets(viewport, scroll)
            .get(owner)
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// Accumulated sticky translation for every node in a sticky subtree.
    /// Frames are stored in DOM preorder, so an outer sticky frame's resolved
    /// movement is available before a nested sticky frame is constrained.
    /// Paint calls this once for the whole document; geometry uses
    /// [`StickyLayout::translation_for`] to avoid an O(DOM) map per query.
    pub fn translations(
        &self,
        viewport: (f32, f32),
        scroll: (f32, f32),
    ) -> HashMap<NodeId, (f32, f32)> {
        let frame_offsets = self.frame_offsets(viewport, scroll);
        self.owners
            .iter()
            .filter_map(|(&id, owner)| frame_offsets.get(owner).copied().map(|offset| (id, offset)))
            .collect()
    }

    /// Sticky-space movement of the ancestor that owns a node's inherited
    /// overflow clip. A sticky descendant must move inside an outer clip,
    /// while a clip established inside the sticky subtree moves with it.
    pub fn clip_translations_from(
        &self,
        translations: &HashMap<NodeId, (f32, f32)>,
    ) -> HashMap<NodeId, (f32, f32)> {
        self.clip_owners
            .iter()
            .filter_map(|(&id, owner)| translations.get(owner).copied().map(|offset| (id, offset)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

pub(crate) fn root_only_cumulative_scroll(scroll_tree: &ScrollTree, scroll: (f32, f32)) -> Vec<(f32, f32)> {
    let mut cumulative = vec![(0.0, 0.0); scroll_tree.containers.len()];
    if let Some(root) = cumulative.first_mut() {
        *root = (-scroll.0, -scroll.1);
    }
    for index in 1..cumulative.len() {
        cumulative[index] = scroll_tree.containers[index]
            .parent
            .map(|parent| cumulative[parent.index()])
            .unwrap_or((0.0, 0.0));
    }
    cumulative
}

pub(crate) fn resolve_sticky_inset(value: Option<crate::Dimension>, basis: f32) -> Option<f32> {
    match value {
        Some(crate::Dimension::Px(px)) => Some(px),
        Some(crate::Dimension::Percent(percent)) => Some(percent * basis),
        _ => None,
    }
}

pub(crate) fn resolve_frame_sticky_inset(
    frame: &StickyFrame,
    index: usize,
    percent_basis: f32,
    viewport: (f32, f32),
) -> Option<f32> {
    if let Some(expression) = frame.inset_expressions[index].as_deref() {
        return crate::style::resolve_contextual_length(
            expression,
            frame.font_size,
            frame.root_font_size,
            viewport.0 / 100.0,
            viewport.1 / 100.0,
            percent_basis,
        );
    }
    resolve_sticky_inset(frame.inset[index], percent_basis)
}

pub(crate) fn sticky_axis_position(
    normal: f32,
    size: f32,
    contain_min: f32,
    contain_max: f32,
    scroll: f32,
    viewport: f32,
    start: Option<f32>,
    end: Option<f32>,
    end_is_inline_start: bool,
) -> f32 {
    let mut stick_start = start.map(|inset| scroll + inset);
    let mut stick_end = end.map(|inset| scroll + viewport - inset - size);

    // When both insets leave a sticky view rectangle smaller than the box,
    // the physical end inset is reduced in LTR, while the physical start
    // inset is reduced in RTL so the logical inline-start edge wins.
    if let (Some(start), Some(end)) = (stick_start, stick_end) {
        if end < start {
            if end_is_inline_start {
                stick_start = Some(end);
            } else {
                stick_end = Some(start);
            }
        }
    }

    let mut position = normal;
    if let Some(start) = stick_start.take() {
        position = position.max(start.min(contain_max));
    }
    if let Some(end) = stick_end {
        position = position.min(end.max(contain_min));
    }
    position
}

