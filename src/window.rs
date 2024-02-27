use std::time::Instant;

use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use gtk::{prelude::*, StringObject};
use itertools::Itertools;

use crate::application::TexApplication;
use crate::config::PROFILE;
use crate::symbol_item::SymbolItem;

mod imp {
    use std::cell::{OnceCell, RefCell};

    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/fyi/zoey/TeX-Match/ui/window.ui")]
    pub struct TeXMatchWindow {
        #[template_child]
        pub drawing_area: TemplateChild<gtk::DrawingArea>,
        #[template_child]
        pub symbol_list: TemplateChild<gtk::ListBox>,
        pub surface: RefCell<Option<cairo::ImageSurface>>,
        pub symbols: OnceCell<gio::ListStore>,
        pub strokes: RefCell<Vec<detexify::Stroke>>,
        pub current_stroke: RefCell<detexify::Stroke>,
        pub sender: OnceCell<std::sync::mpsc::Sender<Vec<detexify::Stroke>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TeXMatchWindow {
        const NAME: &'static str = "TeXMatchWindow";
        type Type = super::TeXMatchWindow;
        type ParentType = gtk::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_instance_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TeXMatchWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Devel Profile
            if PROFILE == "Devel" {
                // Causes GTK_CRITICAL: investigae
                // obj.add_css_class("devel");
            }

            obj.setup_symbol_list();
            obj.setup_drawing_area();
            obj.setup_classifier();
        }

        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for TeXMatchWindow {}
    impl WindowImpl for TeXMatchWindow {}

    impl ApplicationWindowImpl for TeXMatchWindow {}
}

glib::wrapper! {
    pub struct TeXMatchWindow(ObjectSubclass<imp::TeXMatchWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow,
        @implements gio::ActionMap, gio::ActionGroup, gtk::Root;
}

#[gtk::template_callbacks]
impl TeXMatchWindow {
    pub fn new(app: &TexApplication) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// Returns the symbols list store object.
    fn symbols(&self) -> &gio::ListStore {
        self.imp().symbols.get().expect("Failed to get symbols")
    }

    fn setup_symbol_list(&self) {
        let mut model = gio::ListStore::new::<gtk::StringObject>();
        model.extend(
            detexify::iter_symbols()
                .map(|sym| sym.id())
                .map(gtk::StringObject::new),
        );
        // let model: gtk::StringList = detexify::iter_symbols().map(|symbol| symbol.id()).collect();
        tracing::debug!("Loaded {} symbols", model.n_items());

        self.imp()
            .symbols
            .set(model.clone())
            .expect("Failed to set symbol model");

        let selection_model = gtk::NoSelection::new(Some(model));
        self.imp().symbol_list.bind_model(
            Some(&selection_model),
            glib::clone!(@weak self as window => @default-panic, move |obj| {
                let symbol_object = obj.downcast_ref::<StringObject>().expect("The object is not of type `StringObject`.");
                let symbol_item = SymbolItem::new(detexify::Symbol::from_id(symbol_object.string().as_str()).expect("Failed to get symbol"));
                symbol_item.upcast()
            }),
        );

        self.imp().symbol_list.set_visible(true);
    }

    fn setup_classifier(&self) {
        let (req_tx, req_rx) = std::sync::mpsc::channel();
        let (res_tx, res_rx) = async_channel::bounded(1);
        self.imp().sender.set(req_tx).expect("Failed to set tx");
        gio::spawn_blocking(move || {
            tracing::info!("Classifier thread started");
            let classifier = detexify::Classifier::default();

            loop {
                let Some(strokes) = req_rx.iter().next() else {
                    //channel has hung up, cleanly exit
                    tracing::info!("Exiting classifier thread");
                    return;
                };

                let classifications: Option<Vec<detexify::Score>> = 'classify: {
                    let Some(sample) = detexify::StrokeSample::new(strokes) else {
                        tracing::warn!("Skipping classification on empty strokes");
                        break 'classify None;
                    };

                    let start = Instant::now();
                    let Some(results) = classifier.classify(sample) else {
                        tracing::warn!("Classifier returned None");
                        break 'classify None;
                    };
                    tracing::info!(
                        "Classification complete in {}ms",
                        start.elapsed().as_millis()
                    );
                    Some(results)
                };

                res_tx
                    .send_blocking(classifications.unwrap_or_default())
                    .expect("Failed to send classifications");
            }
        });

        glib::spawn_future_local(glib::clone!(@weak self as window => async move {
            tracing::debug!("Listening for classifications");
            while let Ok(classifications) = res_rx.recv().await {

                let symbols = window.symbols();
                symbols.remove_all();

                // let objs = classifications.iter().map(|score|gtk::StringObject::new(&score.id)).collect_vec();
                // symbols.extend_from_slice(&objs);

                // swicthing out all 1k symbols takes too long, so only display the first 25
                // TODO: find faster ways and display all
                for symbol in classifications.iter().take(25) {
                    symbols.append(&gtk::StringObject::new(&symbol.id))
                }
            }
        }));
    }

    fn classify(&self) {
        let imp = self.imp();
        let strokes = imp.strokes.borrow().clone();
        imp.sender
            .get()
            .unwrap()
            .send(strokes)
            .expect("Failed to send strokes");
    }

    fn create_surface(&self, width: i32, height: i32) {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
            .expect("Failed to create surface");
        self.imp().surface.replace(Some(surface));
    }

    fn setup_drawing_area(&self) {
        let imp = self.imp();
        imp.drawing_area.connect_resize(
            glib::clone!(@weak self as window => move |_area: &gtk::DrawingArea, width, height| {
                //recreate surface on size change
                //this shouldn't happen, since the window size is unchangable
                window.create_surface(width, height);
            }),
        );

        let drag = gtk::GestureDrag::builder().button(0).build();
        drag.connect_drag_begin(
            glib::clone!(@weak self as window => move |_drag: &gtk::GestureDrag, x: f64, y: f64 | {
                tracing::trace!("Drag started at {},{}", x, y);
                window.imp().current_stroke.borrow_mut().add_point(detexify::Point {x, y});
                window.imp().drawing_area.queue_draw();
            }),
        );
        drag.connect_drag_update(
            glib::clone!(@weak self as window => move |_drag: &gtk::GestureDrag, x: f64, y: f64 | {
                tracing::trace!("Drag update at {},{}", x, y);
                let mut stroke = window.imp().current_stroke.borrow_mut();
                //x,y refers to movements relative to start coord
                let detexify::Point {x: prev_x, y: prev_y} = stroke.points().next().copied().unwrap();
                stroke.add_point(detexify::Point {x: prev_x + x, y: prev_y + y});
                window.imp().drawing_area.queue_draw();
            }),
        );

        drag.connect_drag_end(
            glib::clone!(@weak self as window => move |_drag: &gtk::GestureDrag, x: f64, y: f64 | {
                tracing::trace!("Drag end at {},{}", x, y);
                let stroke = window.imp().current_stroke.take();
                window.imp().strokes.borrow_mut().push(stroke);
                window.imp().drawing_area.queue_draw();
                //TODO: trigger classifier
                window.classify();

            }),
        );
        imp.drawing_area.add_controller(drag);

        imp.drawing_area.set_draw_func(
            glib::clone!(@weak self as window => move |_area: &gtk::DrawingArea, ctx: &cairo::Context, width, height| {
                if let Some(surface) = window.imp().surface.take() {
                    ctx.set_source_surface(&surface, 0.0, 0.0).expect("Failed to set surface");

                    let curr_stroke = window.imp().current_stroke.borrow().clone();
                    for stroke in window.imp().strokes.borrow().iter().chain(std::iter::once(&curr_stroke)) {
                        tracing::trace!("Drawing: {:?}", stroke);
                        let mut looped = false;
                        for (p, q) in stroke.points().cloned().tuple_windows() {
                            ctx.set_line_width(3.0);
                            ctx.set_source_rgb(0.8, 0.8, 0.8);
                            ctx.set_line_cap(cairo::LineCap::Round);
                            ctx.move_to(p.x, p.y);
                            ctx.line_to(q.x, q.y);
                            ctx.stroke().expect("Failed to set stroke");
                            looped = true;
                        }

                        if !looped && stroke.points().count() == 1 {
                            let p = stroke.points().next().unwrap();
                            ctx.set_source_rgb(0.8, 0.8, 0.8);
                            ctx.arc(p.x, p.y, 1.5, 0.0, 2.0 * std::f64::consts::PI);
                            ctx.fill().expect("Failed to fill");
                        }
                    }
                    window.imp().surface.replace(Some(surface));
                }
            }
        ));
    }

    #[template_callback]
    fn clear(&self, _button: &gtk::Button) {
        // recreate drawing area
        let width = self.imp().drawing_area.content_width();
        let height = self.imp().drawing_area.content_height();
        self.create_surface(width, height);

        //clear previous strokes
        self.imp().strokes.borrow_mut().clear();
        self.imp().current_stroke.borrow_mut().clear();

        self.imp().drawing_area.queue_draw();
    }
}
