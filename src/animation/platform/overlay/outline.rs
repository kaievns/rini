//! The outlines the overview draws of workspaces with no windows: each its tiling area ringed in the
//! focus ring's band, in its active paint while its row is selected and its inactive one otherwise,
//! hung in a container of the tree so it moves and zooms with that container.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2_core_foundation::CGPoint;
use objc2_quartz_core::CALayer;
use rini_geometry::space::{Respace, ToCg};

use crate::animation::domain::choreography::overview::Outline;
use crate::animation::domain::motion::plan::GroupKey;
use crate::decorations::domain::shape::{self, Laying};
use crate::decorations::domain::style::{Band, Decoration, Ring, Worn};
use crate::decorations::platform::layers::Bands;

/// The outlines drawn: each a holder at its frame, carrying its bands.
#[derive(Default)]
pub(super) struct Outlines {
    drawn: Vec<(Retained<CALayer>, Bands)>,
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
            let laying = Laying::on_edge(0.0, 0.0, shape::widths(&decoration, scale));
            bands.place(&laying.fit(outline.frame.size.respace()));
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
            self.drawn.push((holder, bands));
        }
    }

    pub(super) fn clear(&mut self) {
        for (holder, _) in self.drawn.drain(..) {
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
            .map(|(holder, bands)| {
                (
                    holder.clone(),
                    bands.ring.as_ref().map(|band| band.layer.clone()),
                )
            })
            .collect()
    }
}
