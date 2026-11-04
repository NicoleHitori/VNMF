// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! `AspectPicture` — an image whose height is always width × aspect ratio.
//!
//! A plain `gtk::Picture` with `can-shrink` has a minimum height of 0, and `gtk::ListView`
//! gives rows exactly the minimum — the webtoon strip collapses. This widget asks the
//! layout for exactly the height its width requires (height-for-width),
//! even before the image is decoded.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use std::cell::{Cell, OnceCell};

    use super::*;

    #[derive(Default)]
    pub struct AspectPicture {
        pub picture: OnceCell<gtk::Picture>,
        /// Natural width (px) and the image's height/width ratio.
        pub natural_width: Cell<i32>,
        /// Width of the image itself (px).
        pub image_width: Cell<i32>,
        /// Exact size (width, height) if set: then the aspect ratio is not used.
        pub fixed: Cell<Option<(i32, i32)>>,
        pub ratio: Cell<f64>,
        /// How many px below its bounds the image is drawn (see `set_bleed`).
        pub bleed: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AspectPicture {
        const NAME: &'static str = "VnmfAspectPicture";
        type Type = super::AspectPicture;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for AspectPicture {
        fn constructed(&self) {
            self.parent_constructed();
            let picture = gtk::Picture::new();
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Fill);
            picture.set_parent(&*self.obj());
            self.picture.set(picture).unwrap();
            self.ratio.set(1.0);
        }

        fn dispose(&self) {
            if let Some(picture) = self.picture.get() {
                picture.unparent();
            }
        }
    }

    impl WidgetImpl for AspectPicture {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if let Some((w, h)) = self.fixed.get() {
                let size = if orientation == gtk::Orientation::Horizontal { w } else { h };
                return (size, size, -1, -1);
            }
            match orientation {
                gtk::Orientation::Horizontal => (0, self.natural_width.get().max(1), -1, -1),
                // Height is only known for a specific width; without one the minimum is 0
                // (otherwise the minimum could exceed the natural height, which violates the GTK contract).
                _ if for_size < 0 => {
                    let natural = (f64::from(self.natural_width.get()) * self.ratio.get()).round() as i32;
                    (0, natural, -1, -1)
                }
                _ => {
                    let height = (f64::from(for_size) * self.ratio.get()).round() as i32;
                    (height, height, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(picture) = self.picture.get() {
                picture.allocate(width, height + self.bleed.get(), baseline, None);
            }
        }
    }
}

glib::wrapper! {
    pub struct AspectPicture(ObjectSubclass<imp::AspectPicture>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AspectPicture {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Aspect ratio and natural width — from the image dimensions (before decoding).
    pub fn set_dimensions(&self, width: u32, height: u32) {
        let imp = self.imp();
        let w = i32::try_from(width.max(1)).unwrap_or(i32::MAX);
        imp.image_width.set(w);
        imp.natural_width.set(w);
        imp.ratio.set(f64::from(height.max(1)) / f64::from(width.max(1)));
        self.queue_resize();
    }

    /// Exact widget size in px (minimum = natural), regardless of aspect ratio.
    pub fn set_fixed_size(&self, width: i32, height: i32) {
        self.imp().fixed.set(Some((width.max(1), height.max(1))));
        self.queue_resize();
    }

    /// Preferred width (e.g. the webtoon strip width), not less than the image width:
    /// containers such as `adw::Clamp` then get a consistent minimum and natural size.
    /// May also shrink (scaling the strip down).
    pub fn set_natural_width(&self, width: i32) {
        let imp = self.imp();
        let natural = width.max(imp.image_width.get());
        if imp.natural_width.replace(natural) != natural {
            self.queue_resize();
        }
    }

    /// Draw the image `px` below its own bounds — under the next strip fragment. The boundary between
    /// fragments at a fractional position (scrolling, zoom, HiDPI) is antialiased semi-transparently, and the
    /// background showed through it — a thin line between fragments; now the image continues there.
    /// The widget size (and layout) doesn't change; the next fragment is drawn on top.
    pub fn set_bleed(&self, px: i32) {
        if self.imp().bleed.replace(px.max(0)) != px.max(0) {
            self.queue_allocate();
        }
    }

    pub fn set_paintable(&self, paintable: Option<&impl IsA<gtk::gdk::Paintable>>) {
        self.imp().picture.get().unwrap().set_paintable(paintable);
    }

    pub fn picture(&self) -> &gtk::Picture {
        self.imp().picture.get().unwrap()
    }
}

impl Default for AspectPicture {
    fn default() -> Self {
        Self::new()
    }
}
