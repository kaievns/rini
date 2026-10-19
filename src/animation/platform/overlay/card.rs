//! The overview's thumbs drawn as the card (`choreography::overview::card`): while the overview
//! holds the overlay every tile is laid as its card dress has it, its picture clipped to that
//! corner, the selected one's wash drawn under it in the card's `WASH`. Along the zoom each piece
//! plays its dress at every frame, and once the zoom ends the dress its models give; the outlines
//! of empty workspaces keep their width on screen with the zoom.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::CGColor;
use objc2_quartz_core::CALayer;
use rini_core::ids::WindowId;
use rini_geometry::space::{Respace, TileSize, ToCg};

use super::resize::{
    Sampled, changing, install_pieces, install_tile_resize, keyframes, remove_resize, resizes,
    ride_as,
};
use super::{TILE_SIZE_KEY, Tile, Tree, as_frame, number_object, point_object, rect_object};
use crate::animation::domain::choreography::overview::card::{
    self, Dress, Thumb, Wash, Zoomed, dressed, dressed_over, dressing,
};
use crate::animation::domain::motion::score::{Channel, Node};
use crate::chrome::domain::card::WASH;
use crate::decorations::domain::shape::{Fit, Laying, Widths};
use crate::decorations::domain::style::Ring;
use crate::decorations::platform::layers::{Resize, add_under, remove_under, round_continuous};

/// The wash's depth against its thumb's picture: a quarter step behind it, in front of the casters
/// half a step behind it.
const WASH_Z: f64 = -0.25;

const WASH_OPACITY_KEY: &str = "rini.card.wash.opacity";
const WASH_MOVE_KEY: &str = "rini.card.wash.move";
const WASH_SIZE_KEY: &str = "rini.card.wash.size";
const WASH_CORNER_KEY: &str = "rini.card.wash.corner";
const WASH_KEYS: [&str; 4] = [
    WASH_OPACITY_KEY,
    WASH_MOVE_KEY,
    WASH_SIZE_KEY,
    WASH_CORNER_KEY,
];

/// The card's wash under a thumb: a holder riding the thumb's frame, which draws nothing, and the
/// wash in it, reaching out of the thumb.
pub(super) struct Card {
    holder: Retained<CALayer>,
    wash: Retained<CALayer>,
    /// The wash as last placed, which a resize of its thumb carries.
    placed: Cell<Wash>,
}

impl Card {
    /// Under `container` at `frame`, showing no wash.
    fn new(container: &CALayer, frame: CGRect) -> Card {
        let holder = CALayer::layer();
        holder.setAnchorPoint(CGPoint::new(0.0, 0.0));
        holder.setFrame(frame);
        let wash = CALayer::layer();
        wash.setAnchorPoint(CGPoint::new(0.0, 0.0));
        round_continuous(&wash);
        let (red, green, blue) = WASH.rgb();
        wash.setBackgroundColor(Some(&CGColor::new_srgb(red, green, blue, 1.0)));
        wash.setOpacity(0.0);
        holder.addSublayer(&wash);
        container.addSublayer(&holder);
        let placed = Cell::new(Wash {
            share: 0.0,
            out: 0.0,
            corner: 0.0,
        });
        Card { holder, wash, placed }
    }

    /// The layer that rides the thumb's frame.
    pub(super) fn layer(&self) -> &CALayer { &self.holder }

    /// The wash under a thumb drawn at depth `z`.
    pub(super) fn stack(&self, z: f64) { self.holder.setZPosition(z + WASH_Z) }

    /// The wash as `wash` has it around a thumb of `size`.
    fn place(&self, size: TileSize, wash: Wash) {
        self.placed.set(wash);
        self.wash.setFrame(wash_frame(size, wash));
        self.wash.setCornerRadius(wash.corner);
        self.wash.setOpacity(wash.share as f32);
    }

    /// The wash through `frames` from `begin` over `seconds`, each piece only while it changes, and
    /// none while none of it shows.
    fn install(&self, (begin, seconds): (f64, f64), frames: &[(TileSize, Dress)]) {
        if frames.iter().all(|(_, dress)| dress.wash.share <= 0.0) {
            remove_under(&self.wash, &WASH_KEYS);
            return;
        }
        let frame = |(size, dress): &(TileSize, Dress)| wash_frame(*size, dress.wash);
        let pieces: [(&str, &str, Vec<Retained<AnyObject>>); 4] = [
            (
                WASH_OPACITY_KEY,
                "opacity",
                frames.iter().map(|(_, dress)| number_object(dress.wash.share)).collect(),
            ),
            (
                WASH_MOVE_KEY,
                "position",
                frames.iter().map(|frame_of| point_object(frame(frame_of).origin)).collect(),
            ),
            (
                WASH_SIZE_KEY,
                "bounds",
                frames
                    .iter()
                    .map(|frame_of| {
                        rect_object(CGRect::new(CGPoint::new(0.0, 0.0), frame(frame_of).size))
                    })
                    .collect(),
            ),
            (
                WASH_CORNER_KEY,
                "cornerRadius",
                frames.iter().map(|(_, dress)| number_object(dress.wash.corner)).collect(),
            ),
        ];
        let changes = [
            changing(frames, |(_, dress)| dress.wash.share),
            changing(frames, |frame_of| frame(frame_of).origin.x)
                || changing(frames, |frame_of| frame(frame_of).origin.y),
            changing(frames, |frame_of| frame(frame_of).size.width)
                || changing(frames, |frame_of| frame(frame_of).size.height),
            changing(frames, |(_, dress)| dress.wash.corner),
        ];
        for ((key, key_path, values), changes) in pieces.into_iter().zip(changes) {
            if changes {
                add_under(&self.wash, &keyframes(key_path, values, (begin, seconds)), key);
            } else {
                remove_under(&self.wash, &[key]);
            }
        }
    }

    /// The wash as last placed riding `resize` of its thumb, its size at each fit; none of it riding
    /// anything while none of it shows.
    pub(super) fn ride(&self, resize: &impl Resize) {
        remove_under(&self.wash, &[WASH_OPACITY_KEY, WASH_MOVE_KEY, WASH_CORNER_KEY]);
        let wash = self.placed.get();
        if wash.share <= 0.0 {
            remove_under(&self.wash, &[WASH_SIZE_KEY]);
            return;
        }
        let bounds = resize.rect("bounds", |fit| {
            CGRect::new(CGPoint::new(0.0, 0.0), wash_frame(fit.size, wash).size)
        });
        add_under(&self.wash, &bounds, WASH_SIZE_KEY);
    }

    /// Takes away whatever its wash rides.
    pub(super) fn remove_resize(&self) { remove_under(&self.wash, &WASH_KEYS); }

    pub(super) fn remove_animations(&self) {
        self.holder.removeAllAnimations();
        self.wash.removeAllAnimations();
    }

    /// The wash, for the tests.
    #[cfg(test)]
    pub(super) fn wash(&self) -> &CALayer { &self.wash }
}

/// Where the wash lies around a thumb of `size`: `wash.out` out of it on every side.
fn wash_frame(size: TileSize, wash: Wash) -> CGRect {
    let size = size.to_cg();
    CGRect::new(
        CGPoint::new(-wash.out, -wash.out),
        objc2_core_foundation::CGSize::new(
            size.width + 2.0 * wash.out,
            size.height + 2.0 * wash.out,
        ),
    )
}

impl Tile {
    /// The thumb it is drawn as: as its window lays it, its picture keeping the window's own corner
    /// unless clipped, and selected while it wears the active ring.
    pub(super) fn thumb(&self) -> Thumb {
        Thumb {
            own: self.decorations.own(),
            keeps_corner: !self.clipped,
            selected: self.worn.wears_ring == Ring::Active,
        }
    }

    /// Its card, made beside it riding its frame and whatever its frame plays, unless it has one.
    fn card(&mut self) -> &Card {
        let picture = &self.picture;
        self.card.get_or_insert_with(|| {
            let container = picture.superlayer().unwrap_or_else(|| picture.clone());
            let card = Card::new(&container, picture.frame());
            card.stack(picture.zPosition());
            ride_as(card.layer(), picture);
            card
        })
    }
}

impl Tree {
    /// The overview holds the overlay under a zoom by `under` until it lets its tiles go: every
    /// lift cast under it, and every tile drawn as the card from here.
    pub(super) fn hold(&mut self, under: f64) {
        self.held = Some(under);
        for entry in self.tile_layers.values_mut() {
            entry.recast_lift(under);
        }
        let windows: Vec<WindowId> = self.tile_layers.keys().copied().collect();
        self.dress_cards(&windows);
    }

    /// Each of `windows`' tiles dressed as the card while the overview holds the overlay, and
    /// every tile once the last dress along a zoom has ended, so none keeps what it played.
    pub(super) fn dress_cards(&mut self, windows: &[WindowId]) {
        if self.held.is_none() {
            return;
        }
        let windows = match self.dressed_until {
            Some(end) if end <= self.now => {
                self.dressed_until = None;
                self.tile_layers.keys().copied().collect()
            }
            _ => windows.to_vec(),
        };
        for window in windows {
            self.dress_card(window);
        }
    }

    /// `window`'s tile laid as its card dress has it once the replica's tracks end, its picture
    /// clipped, the wash under it as selected or not, the one wearing the active ring selected;
    /// along the zoom while it plays, its dress at every sample, its lift's shape with it; else
    /// while its own track resizes it, that resize laid again as it is laid now, its lift and its
    /// wash with it; and else nothing riding any of it.
    fn dress_card(&mut self, window: WindowId) {
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        let thumb = entry.thumb();
        let held = dressed(&self.replica, window, thumb);
        entry.decorations.lay(Some(held.laying));
        entry.card();
        entry.picture.setMasksToBounds(true);
        let model = self.replica.model(Node::Tile(window)).map(as_frame);
        if let Some(frame) = model {
            self.place_tile(window, frame);
        }
        let Some(entry) = self.tile_layers.get(&window) else {
            return;
        };
        let Some(card) = &entry.card else { return };
        if let Some(frame) = model {
            card.place(frame.size.respace(), held.wash);
        }
        let frames = dressing(&self.replica, window, self.now)
            .map(|span| (span, dressed_over(&self.replica, window, thumb, span)))
            .filter(|(_, frames)| frames.len() > 1);
        let live = self
            .replica
            .track(Node::Tile(window), Channel::Base)
            .filter(|track| track.seconds() > 0.0 && resizes(track) && track.ends() > self.now);
        let until = match (frames, live) {
            (Some(((begin, end), frames)), live) => {
                let fits: Vec<Fit> =
                    frames.iter().map(|(size, dress)| dress.laying.fit(*size)).collect();
                install_pieces(entry, &Sampled {
                    begin,
                    seconds: end - begin,
                    fits: &fits,
                });
                card.install((begin, end - begin), &frames);
                if live.is_none() {
                    for layer in entry.layers() {
                        remove_under(layer, &[TILE_SIZE_KEY]);
                    }
                }
                Some(end)
            }
            (None, Some(track)) => {
                install_tile_resize(entry, track);
                Some(track.ends())
            }
            (None, None) => {
                remove_resize(entry);
                None
            }
        };
        if let Some(end) = until {
            self.dressed_until = Some(self.dressed_until.map_or(end, |until| until.max(end)));
        }
    }

    /// The outlines of empty workspaces as wide on screen under the replica's zoom as at rest, and
    /// along its zoom at every frame.
    pub(super) fn dress_outlines(&self) {
        let (width, zoomed) = card::outlined(&self.replica, self.outlines.width());
        self.outlines.dress(width, zoomed.as_ref());
    }
}

/// An outline's laying with its ring `width` wide: square, the ring on its edge.
pub(super) fn outline_laying(width: f64) -> Laying {
    Laying::on_edge(0.0, 0.0, Widths { border: 0.0, ring: width })
}

/// The fits an outline of `size` passes through along `zoomed`, and how they play.
pub(super) fn outline_fits(size: TileSize, zoomed: &Zoomed) -> Vec<Fit> {
    zoomed.widths.iter().map(|width| outline_laying(*width).fit(size)).collect()
}

/// Every band of `bands` carried along `zoomed` through `fits`.
pub(super) fn install_outline(
    bands: &crate::decorations::platform::layers::Bands,
    zoomed: &Zoomed,
    fits: &[Fit],
) {
    let sampled = Sampled {
        begin: zoomed.begin,
        seconds: zoomed.end - zoomed.begin,
        fits,
    };
    bands.install_resize(&sampled);
}
