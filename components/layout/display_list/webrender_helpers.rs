/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// TODO(gw): This contains helper traits and implementations for converting Servo display lists
//           into WebRender display lists. In the future, this step should be completely removed.
//           This might be achieved by sharing types between WR and Servo display lists, or
//           completely converting layout to directly generate WebRender display lists, for example.

use app_units::Au;
use display_list::ToLayout;
use euclid::Point2D;
use gfx::display_list::{BorderDetails, BorderRadii, ClipScrollNode};
use gfx::display_list::{ClipScrollNodeIndex, ClipScrollNodeType, ClippingRegion, DisplayItem};
use gfx::display_list::{DisplayList, StackingContextType};
use msg::constellation_msg::PipelineId;
use webrender_api::{self, ClipAndScrollInfo, ClipId, ClipMode, ComplexClipRegion};
use webrender_api::{DisplayListBuilder, LayoutTransform};

pub trait WebRenderDisplayListConverter {
    fn convert_to_webrender(&self, pipeline_id: PipelineId) -> DisplayListBuilder;
}

trait WebRenderDisplayItemConverter {
    fn prim_info(&self) -> webrender_api::LayoutPrimitiveInfo;
    fn convert_to_webrender(
        &self,
        builder: &mut DisplayListBuilder,
        clip_scroll_nodes: &[ClipScrollNode],
        clip_ids: &mut Vec<Option<ClipId>>,
        current_clip_and_scroll_info: &mut ClipAndScrollInfo,
    );
}

pub trait ToBorderRadius {
    fn to_border_radius(&self) -> webrender_api::BorderRadius;
}

impl ToBorderRadius for BorderRadii<Au> {
    fn to_border_radius(&self) -> webrender_api::BorderRadius {
        webrender_api::BorderRadius {
            top_left: self.top_left.to_layout(),
            top_right: self.top_right.to_layout(),
            bottom_left: self.bottom_left.to_layout(),
            bottom_right: self.bottom_right.to_layout(),
        }
    }
}

impl WebRenderDisplayListConverter for DisplayList {
    fn convert_to_webrender(&self, pipeline_id: PipelineId) -> DisplayListBuilder {
        let mut builder = DisplayListBuilder::with_capacity(
            pipeline_id.to_webrender(),
            self.bounds().size.to_layout(),
            1024 * 1024,
        ); // 1 MB of space

        let mut current_clip_and_scroll_info = pipeline_id.root_clip_and_scroll_info();
        builder.push_clip_and_scroll_info(current_clip_and_scroll_info);

        let mut clip_ids = Vec::with_capacity(self.clip_scroll_nodes.len());
        clip_ids.resize(self.clip_scroll_nodes.len(), None);
        clip_ids[0] = Some(ClipId::root_scroll_node(pipeline_id.to_webrender()));

        for item in &self.list {
            item.convert_to_webrender(
                &mut builder,
                &self.clip_scroll_nodes,
                &mut clip_ids,
                &mut current_clip_and_scroll_info,
            );
        }
        builder
    }
}

impl WebRenderDisplayItemConverter for DisplayItem {
    fn prim_info(&self) -> webrender_api::LayoutPrimitiveInfo {
        let tag = match self.base().metadata.pointing {
            Some(cursor) => Some((self.base().metadata.node.0 as u64, cursor)),
            None => None,
        };
        webrender_api::LayoutPrimitiveInfo {
            rect: self.base().bounds.to_layout(),
            local_clip: self.base().local_clip,
            // TODO(gw): Make use of the WR backface visibility functionality.
            is_backface_visible: true,
            tag,
        }
    }

    fn convert_to_webrender(
        &self,
        builder: &mut DisplayListBuilder,
        clip_scroll_nodes: &[ClipScrollNode],
        clip_ids: &mut Vec<Option<ClipId>>,
        current_clip_and_scroll_info: &mut ClipAndScrollInfo,
    ) {
        let get_id = |clip_ids: &[Option<ClipId>], index: ClipScrollNodeIndex| -> ClipId {
            match clip_ids[index.0] {
                Some(id) => id,
                None => unreachable!("Tried to use WebRender ClipId before it was defined."),
            }
        };

        let clip_and_scroll_indices = self.base().clipping_and_scrolling;
        let scrolling_id = get_id(clip_ids, clip_and_scroll_indices.scrolling);
        let clip_and_scroll_info = match clip_and_scroll_indices.clipping {
            None => ClipAndScrollInfo::simple(scrolling_id),
            Some(index) => ClipAndScrollInfo::new(scrolling_id, get_id(clip_ids, index)),
        };

        if clip_and_scroll_info != *current_clip_and_scroll_info {
            builder.pop_clip_id();
            builder.push_clip_and_scroll_info(clip_and_scroll_info);
            *current_clip_and_scroll_info = clip_and_scroll_info;
        }

        match *self {
            DisplayItem::SolidColor(ref item) => {
                builder.push_rect(&self.prim_info(), item.color);
            },
            DisplayItem::Text(ref item) => {
                let mut origin = item.baseline_origin.clone();
                let mut glyphs = vec![];

                for slice in item.text_run
                    .natural_word_slices_in_visual_order(&item.range)
                {
                    for glyph in slice.glyphs.iter_glyphs_for_byte_range(&slice.range) {
                        let glyph_advance = if glyph.char_is_space() {
                            glyph.advance() + item.text_run.extra_word_spacing
                        } else {
                            glyph.advance()
                        };
                        if !slice.glyphs.is_whitespace() {
                            let glyph_offset = glyph.offset().unwrap_or(Point2D::zero());
                            let x = (origin.x + glyph_offset.x).to_f32_px();
                            let y = (origin.y + glyph_offset.y).to_f32_px();
                            let point = webrender_api::LayoutPoint::new(x, y);
                            let glyph = webrender_api::GlyphInstance {
                                index: glyph.id(),
                                point: point,
                            };
                            glyphs.push(glyph);
                        }
                        origin.x = origin.x + glyph_advance;
                    }
                }

                if glyphs.len() > 0 {
                    builder.push_text(
                        &self.prim_info(),
                        &glyphs,
                        item.text_run.font_key,
                        item.text_color,
                        None,
                    );
                }
            },
            DisplayItem::Image(ref item) => {
                if let Some(id) = item.webrender_image.key {
                    if item.stretch_size.width > 0.0 && item.stretch_size.height > 0.0 {
                        builder.push_image(
                            &self.prim_info(),
                            item.stretch_size,
                            item.tile_spacing,
                            item.image_rendering,
                            webrender_api::AlphaType::PremultipliedAlpha,
                            id,
                        );
                    }
                }
            },
            DisplayItem::Border(ref item) => {
                let details = match item.details {
                    BorderDetails::Normal(ref border) => {
                        webrender_api::BorderDetails::Normal(*border)
                    },
                    BorderDetails::Image(ref image) => webrender_api::BorderDetails::Image(*image),
                    BorderDetails::Gradient(ref gradient) => {
                        webrender_api::BorderDetails::Gradient(webrender_api::GradientBorder {
                            gradient: builder.create_gradient(
                                gradient.gradient.start_point,
                                gradient.gradient.end_point,
                                gradient.gradient.stops.clone(),
                                gradient.gradient.extend_mode,
                            ),
                            outset: gradient.outset,
                        })
                    },
                    BorderDetails::RadialGradient(ref gradient) => {
                        webrender_api::BorderDetails::RadialGradient(
                            webrender_api::RadialGradientBorder {
                                gradient: builder.create_radial_gradient(
                                    gradient.gradient.center,
                                    gradient.gradient.radius,
                                    gradient.gradient.stops.clone(),
                                    gradient.gradient.extend_mode,
                                ),
                                outset: gradient.outset,
                            },
                        )
                    },
                };

                builder.push_border(&self.prim_info(), item.border_widths, details);
            },
            DisplayItem::Gradient(ref item) => {
                let gradient = builder.create_gradient(
                    item.gradient.start_point,
                    item.gradient.end_point,
                    item.gradient.stops.clone(),
                    item.gradient.extend_mode,
                );
                builder.push_gradient(&self.prim_info(), gradient, item.tile, item.tile_spacing);
            },
            DisplayItem::RadialGradient(ref item) => {
                let gradient = builder.create_radial_gradient(
                    item.gradient.center,
                    item.gradient.radius,
                    item.gradient.stops.clone(),
                    item.gradient.extend_mode,
                );
                builder.push_radial_gradient(
                    &self.prim_info(),
                    gradient,
                    item.tile,
                    item.tile_spacing,
                );
            },
            DisplayItem::Line(ref item) => {
                builder.push_line(
                    &self.prim_info(),
                    // TODO(gw): Use a better estimate for wavy line thickness.
                    (0.33 * item.base.bounds.size.height.to_f32_px()).ceil(),
                    webrender_api::LineOrientation::Horizontal,
                    &item.color,
                    item.style,
                );
            },
            DisplayItem::BoxShadow(ref item) => {
                builder.push_box_shadow(
                    &self.prim_info(),
                    item.box_bounds,
                    item.offset,
                    item.color,
                    item.blur_radius,
                    item.spread_radius,
                    item.border_radius,
                    item.clip_mode,
                );
            },
            DisplayItem::PushTextShadow(ref item) => {
                builder.push_shadow(
                    &self.prim_info(),
                    webrender_api::Shadow {
                        blur_radius: item.blur_radius,
                        offset: item.offset,
                        color: item.color,
                    },
                );
            },
            DisplayItem::PopAllTextShadows(_) => {
                builder.pop_all_shadows();
            },
            DisplayItem::Iframe(ref item) => {
                builder.push_iframe(&self.prim_info(), item.iframe.to_webrender());
            },
            DisplayItem::PushStackingContext(ref item) => {
                let stacking_context = &item.stacking_context;
                debug_assert!(stacking_context.context_type == StackingContextType::Real);

                let transform = stacking_context
                    .transform
                    .map(|transform| LayoutTransform::from_untyped(&transform).into());
                let perspective = stacking_context
                    .perspective
                    .map(|perspective| LayoutTransform::from_untyped(&perspective));

                builder.push_stacking_context(
                    &webrender_api::LayoutPrimitiveInfo::new(stacking_context.bounds.to_layout()),
                    stacking_context.scroll_policy,
                    transform,
                    stacking_context.transform_style,
                    perspective,
                    stacking_context.mix_blend_mode,
                    stacking_context.filters.clone(),
                );
            },
            DisplayItem::PopStackingContext(_) => builder.pop_stacking_context(),
            DisplayItem::DefineClipScrollNode(ref item) => {
                let node = &clip_scroll_nodes[item.node_index.0];
                let parent_id = get_id(clip_ids, node.parent_index);
                let item_rect = node.clip.main.to_layout();

                let webrender_id = match node.node_type {
                    ClipScrollNodeType::Clip => builder.define_clip_with_parent(
                        node.id,
                        parent_id,
                        item_rect,
                        node.clip.get_complex_clips(),
                        None,
                    ),
                    ClipScrollNodeType::ScrollFrame(scroll_sensitivity) => builder
                        .define_scroll_frame_with_parent(
                            node.id,
                            parent_id,
                            node.content_rect,
                            node.clip.main.to_layout(),
                            node.clip.get_complex_clips(),
                            None,
                            scroll_sensitivity,
                        ),
                    ClipScrollNodeType::StickyFrame(ref sticky_data) => {
                        // TODO: Add define_sticky_frame_with_parent to WebRender.
                        builder.push_clip_id(parent_id);
                        let id = builder.define_sticky_frame(
                            node.id,
                            item_rect,
                            sticky_data.margins,
                            sticky_data.vertical_offset_bounds,
                            sticky_data.horizontal_offset_bounds,
                            webrender_api::LayoutVector2D::zero(),
                        );
                        builder.pop_clip_id();
                        id
                    },
                };

                debug_assert!(node.id.is_none() || node.id == Some(webrender_id));
                clip_ids[item.node_index.0] = Some(webrender_id);
            },
        }
    }
}

trait ToWebRenderClip {
    fn get_complex_clips(&self) -> Vec<ComplexClipRegion>;
}

impl ToWebRenderClip for ClippingRegion {
    fn get_complex_clips(&self) -> Vec<ComplexClipRegion> {
        self.complex
            .iter()
            .map(|complex_clipping_region| {
                ComplexClipRegion::new(
                    complex_clipping_region.rect.to_layout(),
                    complex_clipping_region.radii.to_border_radius(),
                    ClipMode::Clip,
                )
            })
            .collect()
    }
}
