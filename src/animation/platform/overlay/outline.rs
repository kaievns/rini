//! The outlines the overview draws of workspaces with no windows: each its tiling area ringed in the
//! focus ring's band, in its active paint while its row is selected and its inactive one otherwise,
//! hung in a container of the tree so it moves and zooms with that container, its ring as wide on
//! screen as at rest under the zoom (`Tree::dress_outlines`).

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2_core_foundation::CGPoint;
use objc2_quartz_core::CALayer;
use rini_geometry::space::{Respace, TileSize, ToCg};

use super::card;
use crate::animation::domain::choreography::overview::Outline;
use crate::animation::domain::choreography::overview::card::Zoomed;
use crate::animation::domain::motion::plan::GroupKey;
use crate::decorations::domain::shape;
use crate::decorations::domain::style::{Band, Decoration, Ring, Worn};
use crate::decorations::platform::layers::Bands;

/// The outlines drawn: each a holder at its frame, carrying its bands, and the size it rings; and
/// how wide their ring is on screen.
#[derive(Default)]
pub(super) struct Outlines {
    drawn: Vec<(Retained<CALayer>, Bands, TileSize)>,
    width: f64,
}

impl Outlines {
    /// `outlines` drawn in their containers, ringed as `ring` is, in place of those before; one
    /// whose container is not there, or with no ring to draw, draws nothing.
    pub(super) fn draw(
        &mut self,
        outlines: &[Outline],
        ring: Option<Band>,
        containers: &HashMap<GroupKey, Retained<CALayer>>,
        scale: f64,
    ) {
        self.clear();
        let Some(ring) = ring else { return };
        let decoration = Decoration {
            ring: Some(ring),
            ..Decoration::default()
        };
        self.width = shape::widths(&decoration, scale).ring;
        for outline in outlines {
            let Some(container) = containers.get(&outline.key) else {
                continue;
            };
            let holder = CALayer::layer();
            holder.setAnchorPoint(CGPoint::new(0.0, 0.0));
            holder.setFrame(outline.frame.to_cg());
            container.addSublayer(&holder);
            let mut bands = Bands::new(&holder);
            bands.wear(&decoration, scale);
            let size: TileSize = outline.frame.size.respace();
            bands.place(&card::outline_laying(self.width).fit(size));
            let worn = Worn {
                decoration,
                wears_ring: if outline.selected {
                    Ring::Active
                } else {
                    Ring::Inactive
                },
                holds_focus: outline.selected,
            };
            bands.paint(&worn.style(false));
            self.drawn.push((holder, bands, size));
        }
    }

    /// How wide their ring is on screen, at rest.
    pub(super) fn width(&self) -> f64 { self.width }

    /// Every outline's ring `width` wide once the zoom ends, and along `zoomed` while it plays.
    pub(super) fn dress(&self, width: f64, zoomed: Option<&Zoomed>) {
        for (_, bands, size) in &self.drawn {
            bands.place(&card::outline_laying(width).fit(*size));
            match zoomed {
                Some(zoomed) => {
                    card::install_outline(bands, zoomed, &card::outline_fits(*size, zoomed))
                }
                None => bands.remove_resize(),
            }
        }
    }

    pub(super) fn clear(&mut self) {
        for (holder, ..) in self.drawn.drain(..) {
            holder.removeFromSuperlayer();
        }
    }

    /// Each outline drawn: its holder and its ring's layer, for the tests.
    #[cfg(test)]
    pub(super) fn drawn(
        &self,
    ) -> Vec<(
        Retained<CALayer>,
        Option<Retained<objc2_quartz_core::CAGradientLayer>>,
    )> {
        self.drawn
            .iter()
            .map(|(holder, bands, _)| {
                (
                    holder.clone(),
                    bands.ring.as_ref().map(|band| band.layer.clone()),
                )
            })
            .collect()
    }
}
