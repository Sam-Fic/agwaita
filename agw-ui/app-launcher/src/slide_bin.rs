//! A plain single-child container that draws its child translated upward by
//! (1 - progress) * own height. It exists because GtkRevealer (like CSS
//! transitions) refuses to animate while the frame clock cannot report a
//! frame rate, which is the case for freshly remapped layer surfaces — the
//! launcher would snap instead of sliding on its first open. Driven by an
//! AdwAnimation, which only honors the gtk-enable-animations setting.

use gtk4::{
    gdk,
    glib,
    glib::{
        object_subclass,
        subclass::prelude::*,
    },
    graphene,
    prelude::*,
    subclass::{
        box_::BoxImpl,
        prelude::*,
        widget::WidgetImpl,
    },
};

glib::wrapper! {
    pub struct SlideBin(ObjectSubclass<SlideBinImp>)
     @extends gtk4::Box, gtk4::Widget,
     @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget, gtk4::Orientable;
}

impl SlideBin {
    pub fn new() -> Self {
        glib::Object::new::<SlideBin>()
    }
}

impl Default for SlideBin {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
pub struct SlideBinImp {
    /// 0 = child fully below the bottom edge, 1 = child at rest
    progress: std::cell::Cell<f64>,
}

#[object_subclass]
impl ObjectSubclass for SlideBinImp {
    const NAME: &'static str = "AgwaitaSlideBin";
    type Type = SlideBin;
    type ParentType = gtk4::Box;
}

impl ObjectImpl for SlideBinImp {
    fn properties() -> &'static [glib::ParamSpec] {
        use std::sync::OnceLock;
        static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
        PROPS.get_or_init(|| {
            vec![glib::ParamSpecDouble::builder("progress")
                .minimum(0.0)
                .maximum(1.0)
                .default_value(0.0)
                .build()]
        })
    }

    fn property(&self, id: usize, _pspec: &glib::ParamSpec) -> glib::Value {
        match id {
            1 => self.progress.get().to_value(),
            _ => unimplemented!(),
        }
    }

    fn set_property(&self, _id: usize, value: &glib::Value, _pspec: &glib::ParamSpec) {
        match _id {
            1 => {
                self.progress.set(value.get::<f64>().unwrap_or(0.0));
                self.obj().queue_draw();
            },
            _ => unimplemented!(),
        }
    }
}

impl BoxImpl for SlideBinImp {}

impl WidgetImpl for SlideBinImp {
    fn snapshot(&self, snapshot: &gtk4::Snapshot) {
        let widget = self.obj();
        let progress = self.progress.get() as f32;
        // Semi-transparent pure black backdrop behind the card, fading with
        // the same progress so the dim fades in/out in lockstep with the
        // slide (including mid-flight turn-arounds).
        if progress > 0.0 {
            let dim = gdk::RGBA::new(0.0, 0.0, 0.0, 0.4 * progress);
            snapshot.append_color(
                &dim,
                &graphene::Rect::new(
                    0.0,
                    0.0,
                    widget.width() as f32,
                    widget.height() as f32,
                ),
            );
        }
        let offset = ((1.0 - self.progress.get()) * f64::from(widget.height())) as f32;
        if offset != 0.0 {
            snapshot.translate(&graphene::Point::new(0.0, offset));
        }
        for child in widget.observe_children().iter::<glib::Object>().flatten() {
            if let Ok(child) = child.downcast::<gtk4::Widget>() {
                widget.snapshot_child(&child, snapshot);
            }
        }
    }
}

// relm4's view! macro appends children through RelmContainerExt, which is
// only implemented for the GTK containers it knows; teach it that a SlideBin
// behaves like the Box it extends.
impl relm4::ContainerChild for SlideBin {
    type Child = gtk4::Widget;
}

impl relm4::RelmContainerExt for SlideBin {
    fn container_add(&self, widget: &impl AsRef<gtk4::Widget>) {
        self.append(widget.as_ref());
    }
}
