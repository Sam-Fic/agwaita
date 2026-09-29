//! A single-child container that animates its child in and out toward one
//! screen edge, plus a semi-transparent black backdrop that fades with the
//! same progress. Driven by an AdwAnimation through the "progress" property
//! (0 = child fully outside the screen at the chosen edge, 1 = child at
//! rest); hides simply animate the progress back, so mid-flight
//! turn-arounds reverse the whole choreography.
//!
//! It exists because GtkRevealer (like CSS transitions) refuses to animate
//! while the frame clock cannot report a frame rate, which is the case for
//! freshly remapped layer surfaces — popups would snap instead of sliding
//! on their first open. AdwAnimation only honors the gtk-enable-animations
//! setting.

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

/// The screen edge the child travels to/from. Pick the edge the popup is
/// anchored at: a top-anchored popup slides down from the top edge, a
/// bottom- or center-anchored one slides up from the bottom edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlideEdge {
    /// Child rests in place and hides below the bottom edge.
    #[default]
    Bottom,
    /// Child rests in place and hides above the top edge.
    Top,
}

impl SlideBin {
    pub fn new(edge: SlideEdge) -> Self {
        glib::Object::builder().property("from-top", edge == SlideEdge::Top).build()
    }
}

#[derive(Default)]
pub struct SlideBinImp {
    /// 0 = child fully outside the screen at its edge, 1 = child at rest
    progress: std::cell::Cell<f64>,
    /// When true the child hides above the top edge instead of below the
    /// bottom edge.
    from_top: std::cell::Cell<bool>,
    /// Alpha of the pure black dim painted behind the child at full
    /// progress; 0 disables the backdrop entirely (popups whose surface
    /// already paints its own background).
    backdrop_opacity: std::cell::Cell<f64>,
}

#[object_subclass]
impl ObjectSubclass for SlideBinImp {
    const NAME: &'static str = "AgwaitaSlideBin";
    type Type = SlideBin;
    type ParentType = gtk4::Box;
}

impl ObjectImpl for SlideBinImp {
    fn constructed(&self) {
        self.parent_constructed();
        // Stack the child Bin-style: alignment comes from the child's own
        // halign/valign, and a FILL-aligned child fills the bin exactly like
        // a direct child of the window would. A Box's main-axis packing
        // would instead ignore the child's main-axis alignment and drag
        // centered popups to the left edge.
        self.obj().set_layout_manager(Some(gtk4::BinLayout::new()));
    }

    fn properties() -> &'static [glib::ParamSpec] {
        use std::sync::OnceLock;
        static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
        PROPS.get_or_init(|| {
            vec![
                glib::ParamSpecDouble::builder("progress")
                    .minimum(0.0)
                    .maximum(1.0)
                    .default_value(0.0)
                    .build(),
                glib::ParamSpecBoolean::builder("from-top").build(),
                glib::ParamSpecDouble::builder("backdrop-opacity")
                    .minimum(0.0)
                    .maximum(1.0)
                    .default_value(0.4)
                    .build(),
            ]
        })
    }

    fn property(&self, id: usize, _pspec: &glib::ParamSpec) -> glib::Value {
        match id {
            1 => self.progress.get().to_value(),
            2 => self.from_top.get().to_value(),
            3 => self.backdrop_opacity.get().to_value(),
            _ => unimplemented!(),
        }
    }

    fn set_property(&self, _id: usize, value: &glib::Value, _pspec: &glib::ParamSpec) {
        match _id {
            1 => {
                self.progress.set(value.get::<f64>().unwrap_or(0.0));
                self.obj().queue_draw();
            },
            2 => {
                self.from_top.set(value.get::<bool>().unwrap_or(false));
                self.obj().queue_draw();
            },
            3 => {
                self.backdrop_opacity
                    .set(value.get::<f64>().unwrap_or(0.4));
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
        // Semi-transparent pure black backdrop behind the child, fading with
        // the same progress so the dim fades in/out in lockstep with the
        // slide (including mid-flight turn-arounds).
        let backdrop = (self.backdrop_opacity.get() as f32) * progress;
        if backdrop > 0.0 {
            let dim = gdk::RGBA::new(0.0, 0.0, 0.0, backdrop);
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
        if progress >= 1.0 {
            // Resting state: draw the child untouched, no transform cost.
            for child in widget.observe_children().iter::<glib::Object>().flatten() {
                if let Ok(child) = child.downcast::<gtk4::Widget>() {
                    widget.snapshot_child(&child, snapshot);
                }
            }
            return;
        }

        let direction = if self.from_top.get() { -1.0 } else { 1.0 };
        let offset = direction * (1.0 - progress) * widget.height() as f32;
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
