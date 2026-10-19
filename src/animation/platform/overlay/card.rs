//! The overview's thumbs drawn as the card (`choreography::overview::card`): while the overview
//! holds the overlay every tile but a floating window's is laid as its card dress has it, its
//! picture clipped to that corner, the selected one's wash drawn under it in the card's `WASH` and
//! every other's picture edged in the card's `EDGE`.
//! Along the zoom each piece plays its dress at every frame, and once the zoom ends the dress its
//! models give; the outlines of empty workspaces keep their width on screen with the zoom.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect};
use objc2_core_graphics::CGColor;
use objc2_quartz_core::CALayer;
use rini_core::ids::WindowId;
use rini_geometry::space::{Respace, TileSize, ToCg};

use super::resize::{
    Sampled, changing, install_pieces, install_tile_resize, keyframes, remove_resize, resizes,
    ride_as,
};
use super::{
    TILE_SIZE_KEY, Tile, Tree, as_frame, cf_object, clip_corners, number_object, point_object,
    rect_object,
};
use crate::animation::domain::choreography::overview::card::{
    self, Dress, Edge, Thumb, Wash, Zoomed, dressed, dressed_over, dressing,
};
use crate::animation::domain::motion::plan::GroupKey;
use crate::animation::domain::motion::score::{Channel, Node};
use crate::chrome::domain::card::{EDGE, EDGE_ALPHA, WASH};
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

pub(super) const EDGE_WIDTH_KEY: &str = "rini.card.edge.width";
pub(super) const EDGE_COLOUR_KEY: &str = "rini.card.edge.colour";
const EDGE_KEYS: [&str; 2] = [EDGE_WIDTH_KEY, EDGE_COLOUR_KEY];

/// The card's `EDGE` at `share` of its `EDGE_ALPHA`.
pub(super) fn edge_colour(share: f64) -> CFRetained<CGColor> {
    let (red, green, blue) = EDGE.rgb();
    CGColor::new_srgb(red, green, blue, EDGE_ALPHA * share.clamp(0.0, 1.0))
}

/// A thumb's `picture` edged as `edge` has it: a border, which Core Animation draws over the
/// picture and round its corner.
fn place_edge(picture: &CALayer, edge: Edge) {
    picture.setBorderWidth(edge.width);
    picture.setBorderColor(Some(&edge_colour(edge.share)));
}

/// A thumb's `picture` edged through `frames` from `begin` over `seconds`, each piece only while it
/// changes, and nothing playing while none of it shows.
fn install_edge(picture: &CALayer, (begin, seconds): (f64, f64), frames: &[(TileSize, Dress)]) {
    if frames.iter().all(|(_, dress)| dress.edge.share <= 0.0) {
        remove_under(picture, &EDGE_KEYS);
        return;
    }
    let lay = |key: &str,
               key_path: &str,
               changes: bool,
               values: &dyn Fn() -> Vec<Retained<AnyObject>>| {
        if changes {
            add_under(picture, &keyframes(key_path, values(), (begin, seconds)), key);
        } else {
            remove_under(picture, &[key]);
        }
    };
    lay(
        EDGE_WIDTH_KEY,
        "borderWidth",
        changing(frames, |(_, dress)| dress.edge.width),
        &|| frames.iter().map(|(_, dress)| number_object(dress.edge.width)).collect(),
    );
    lay(
        EDGE_COLOUR_KEY,
        "borderColor",
        changing(frames, |(_, dress)| dress.edge.share),
        &|| {
            frames
                .iter()
                .map(|(_, dress)| cf_object(&edge_colour(dress.edge.share)))
                .collect()
        },
    );
}

/// A thumb's `picture` with no edge, nothing playing on it.
fn take_off_edge(picture: &CALayer) {
    remove_under(picture, &EDGE_KEYS);
    picture.setBorderWidth(0.0);
    picture.setBorderColor(None);
}

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
    /// clipped, the wash under it and the edge over it as selected or not, the one wearing the
    /// active ring selected;
    /// along the zoom while it plays, its dress at every sample, its lift's shape with it; else
    /// while its own track resizes it, that resize laid again as it is laid now, its lift and its
    /// wash with it; and else nothing riding any of it.
    fn dress_card(&mut self, window: WindowId) {
        if self.replica.parent(window) == Some(GroupKey::Floating) {
            self.undress_card(window);
            return;
        }
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
        place_edge(&entry.picture, held.edge);
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
                install_edge(&entry.picture, (begin, end - begin), &frames);
                if live.is_none() {
                    for layer in entry.layers() {
                        remove_under(layer, &[TILE_SIZE_KEY]);
                    }
                }
                Some(end)
            }
            (None, Some(track)) => {
                install_tile_resize(entry, track);
                remove_under(&entry.picture, &EDGE_KEYS);
                Some(track.ends())
            }
            (None, None) => {
                remove_resize(entry);
                remove_under(&entry.picture, &EDGE_KEYS);
                None
            }
        };
        if let Some(end) = until {
            self.dressed_until = Some(self.dressed_until.map_or(end, |until| until.max(end)));
        }
    }

    /// `window`'s tile, riding the floating container, drawn as its window lays it rather than as a
    /// card: a card and an edge it wore as a thumb taken off, its picture clipped as it was given,
    /// and its own resize, where its track has one, laid again so, its frame's resize left playing.
    fn undress_card(&mut self, window: WindowId) {
        let model = self.replica.model(Node::Tile(window)).map(as_frame);
        let resize = self
            .replica
            .track(Node::Tile(window), Channel::Base)
            .filter(|track| track.seconds() > 0.0 && resizes(track));
        let Some(entry) = self.tile_layers.get_mut(&window) else {
            return;
        };
        let Some(card) = entry.card.take() else {
            return;
        };
        card.layer().removeFromSuperlayer();
        take_off_edge(&entry.picture);
        entry.decorations.lay(None);
        clip_corners(entry, entry.clipped);
        match resize {
            Some(track) => install_tile_resize(entry, track),
            None => remove_resize(entry),
        }
        if let Some(frame) = model {
            self.place_tile(window, frame);
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
