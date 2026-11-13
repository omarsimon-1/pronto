//! Action glyphs at full strength where Adwaita's own icons are faint (phase C, the visual QA):
//! its `non-starred-symbolic` and `mail-read-symbolic` paint their shape at 35 % and 50 %
//! opacity, so Star and Mark as Read looked disabled beside Done, Delete and Move (2.4:1 and
//! 3.5:1 on Adwaita dark, where their neighbours read 10:1; the select mode's bar, a row's quick
//! Star). CSS can recolour a symbolic icon but not lift the opacity in its paths, so these two
//! are drawn here: a [`gtk::SymbolicPaintable`], which GTK colours with the CSS colour of the
//! image showing it, at the image's icon size, as it colours a symbolic icon.
//!
//! The shapes are drawn for this app (an outlined five-pointed star, an open envelope), on a 16
//! px grid with a 1.5 px stroke like Adwaita's outlines.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gdk, gio, glib, gsk};
use std::cell::Cell;

/// A glyph drawn here instead of a faint theme icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Glyph {
    /// Star: an outlined star (Adwaita's `non-starred-symbolic`).
    #[default]
    StarOutline,
    /// Mark as Read: an open envelope (Adwaita's `mail-read-symbolic`).
    MailRead,
}

impl Glyph {
    /// The glyph that stands for the theme icon `name`, when that one is faint.
    pub fn for_icon(name: &str) -> Option<Self> {
        match name {
            "non-starred-symbolic" => Some(Glyph::StarOutline),
            "mail-read-symbolic" => Some(Glyph::MailRead),
            _ => None,
        }
    }

    /// The theme icon it stands for.
    pub fn icon_name(self) -> &'static str {
        match self {
            Glyph::StarOutline => "non-starred-symbolic",
            Glyph::MailRead => "mail-read-symbolic",
        }
    }

    /// The glyph's outline on a 16 × 16 grid.
    fn path(self) -> gsk::Path {
        let builder = gsk::PathBuilder::new();
        match self {
            Glyph::StarOutline => {
                // Five points round (8, 8.4): 6.6 px out, 2.9 px in, the first at the top.
                for (index, point) in star_points(8.0, 8.4, 6.6, 2.9).into_iter().enumerate() {
                    if index == 0 {
                        builder.move_to(point.0, point.1);
                    } else {
                        builder.line_to(point.0, point.1);
                    }
                }
                builder.close();
            }
            Glyph::MailRead => {
                // The body with its flap open above it, then the fold from the flap's corners.
                builder.move_to(1.75, 6.5);
                builder.line_to(8.0, 2.25);
                builder.line_to(14.25, 6.5);
                builder.line_to(14.25, 12.75);
                builder.quad_to(14.25, 14.25, 12.75, 14.25);
                builder.line_to(3.25, 14.25);
                builder.quad_to(1.75, 14.25, 1.75, 12.75);
                builder.close();
                builder.move_to(1.75, 6.5);
                builder.line_to(8.0, 10.5);
                builder.line_to(14.25, 6.5);
            }
        }
        builder.to_path()
    }
}

/// A five-pointed star's ten corners, the first point at the top, clockwise.
fn star_points(x: f32, y: f32, outer: f32, inner: f32) -> Vec<(f32, f32)> {
    (0..10)
        .map(|corner| {
            let radius = if corner % 2 == 0 { outer } else { inner };
            let angle = (-90.0 + 36.0 * corner as f32).to_radians();
            (x + radius * angle.cos(), y + radius * angle.sin())
        })
        .collect()
}

/// The stroke of every glyph: Adwaita's outline weight at 16 px, round where lines meet.
const STROKE: f32 = 1.5;
/// The grid the glyphs are drawn on.
const GRID: f64 = 16.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GlyphPaintable {
        pub(super) glyph: Cell<Glyph>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GlyphPaintable {
        const NAME: &'static str = "ProntoGlyphPaintable";
        type Type = super::GlyphPaintable;
        type Interfaces = (gdk::Paintable, gtk::SymbolicPaintable);
    }

    impl ObjectImpl for GlyphPaintable {}

    impl PaintableImpl for GlyphPaintable {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE | gdk::PaintableFlags::STATIC_CONTENTS
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            // Drawn outside an icon (no CSS colour given): the dark grey Adwaita's icons use.
            self.draw(
                snapshot,
                width,
                height,
                &gdk::RGBA::new(0.18, 0.2, 0.2, 1.0),
            );
        }
    }

    impl SymbolicPaintableImpl for GlyphPaintable {
        fn snapshot_symbolic(
            &self,
            snapshot: &gdk::Snapshot,
            width: f64,
            height: f64,
            colors: &[gdk::RGBA],
        ) {
            let color = colors.first().copied().unwrap_or(gdk::RGBA::BLACK);
            self.draw(snapshot, width, height, &color);
        }
    }

    impl GlyphPaintable {
        fn draw(&self, snapshot: &gdk::Snapshot, width: f64, height: f64, color: &gdk::RGBA) {
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else {
                return;
            };
            let stroke = gsk::Stroke::new(STROKE);
            stroke.set_line_join(gsk::LineJoin::Round);
            stroke.set_line_cap(gsk::LineCap::Round);
            snapshot.save();
            snapshot.scale((width / GRID) as f32, (height / GRID) as f32);
            snapshot.append_stroke(&self.glyph.get().path(), &stroke, color);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    /// A [`Glyph`] as a symbolic paintable: an image shows it in its CSS colour.
    pub struct GlyphPaintable(ObjectSubclass<imp::GlyphPaintable>)
        @implements gdk::Paintable, gtk::SymbolicPaintable;
}

impl GlyphPaintable {
    pub fn new(glyph: Glyph) -> Self {
        let paintable: Self = glib::Object::new();
        paintable.imp().glyph.set(glyph);
        paintable
    }

    pub fn glyph(&self) -> Glyph {
        self.imp().glyph.get()
    }
}

/// Shows the icon `names` (the first, with the others as fallbacks) in `image`: a glyph of
/// this module's for one Adwaita draws faint, else the theme's icon.
pub fn set_icon(image: &gtk::Image, names: &[&str]) {
    match names.first().and_then(|name| Glyph::for_icon(name)) {
        Some(glyph) => {
            let shown = image
                .paintable()
                .and_downcast::<GlyphPaintable>()
                .is_some_and(|paintable| paintable.glyph() == glyph);
            if !shown {
                image.set_paintable(Some(&GlyphPaintable::new(glyph)));
            }
        }
        None => image.set_from_gicon(&gio::ThemedIcon::from_names(names)),
    }
}

/// The glyph `image` shows, when it shows one of this module's (the self-test).
pub fn shown(image: &gtk::Image) -> Option<Glyph> {
    image
        .paintable()
        .and_downcast::<GlyphPaintable>()
        .map(|paintable| paintable.glyph())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_faint_icons_are_drawn_here() {
        assert_eq!(
            Glyph::for_icon("non-starred-symbolic"),
            Some(Glyph::StarOutline)
        );
        assert_eq!(Glyph::for_icon("mail-read-symbolic"), Some(Glyph::MailRead));
        for glyph in [Glyph::StarOutline, Glyph::MailRead] {
            assert_eq!(Glyph::for_icon(glyph.icon_name()), Some(glyph));
        }
        for name in [
            "starred-symbolic",
            "mail-unread-symbolic",
            "object-select-symbolic",
            "user-trash-symbolic",
        ] {
            assert_eq!(
                Glyph::for_icon(name),
                None,
                "{name} is drawn at full strength"
            );
        }
    }

    #[test]
    fn the_star_fits_its_grid_with_its_stroke() {
        let points = star_points(8.0, 8.4, 6.6, 2.9);
        assert_eq!(points.len(), 10);
        let margin = STROKE / 2.0;
        for (x, y) in points {
            assert!(
                (margin..=16.0 - margin).contains(&x) && (margin..=16.0 - margin).contains(&y),
                "({x}, {y}) leaves the grid"
            );
        }
        // The top point straight above the centre.
        let (x, y) = star_points(8.0, 8.4, 6.6, 2.9)[0];
        assert!((x - 8.0).abs() < 0.001 && (y - 1.8).abs() < 0.001);
    }
}
