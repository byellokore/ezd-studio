//! macOS window for opening DXF and EzCad drawings and saving `.ezd` or `.dxf`.

use eframe::egui::{
    self, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2,
};
use ezd_studio::{open_drawing, write_dxf, write_ezd, Document, Pen};
use std::path::PathBuf;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([960.0, 640.0])
            .with_title("EZD Studio"),
        ..Default::default()
    };
    eframe::run_native(
        "EZD Studio",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            apply_style(&cc.egui_ctx);
            Ok(Box::new(Studio::new()))
        }),
    )
}

fn install_fonts(ctx: &egui::Context) {
    let path = "/System/Library/Fonts/Supplemental/Arial Unicode.ttf";
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "arial-unicode".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "arial-unicode".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "arial-unicode".to_owned());
    ctx.set_fonts(fonts);
}

fn apply_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgb(24, 26, 30);
    visuals.panel_fill = Color32::from_rgb(18, 20, 24);
    visuals.faint_bg_color = Color32::from_rgb(32, 35, 40);
    visuals.extreme_bg_color = Color32::from_rgb(12, 13, 16);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(32, 35, 40);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 46, 54);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(58, 64, 74);
    visuals.widgets.active.bg_fill = Color32::from_rgb(72, 80, 94);
    visuals.selection.bg_fill = Color32::from_rgb(176, 122, 48);
    visuals.hyperlink_color = Color32::from_rgb(232, 184, 96);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
    });
}

struct View {
    center_x: f64,
    center_y: f64,
    zoom: f64,
}

impl View {
    fn fit(&mut self, doc: &Document, size: Vec2) {
        let (min_x, min_y, max_x, max_y) = if let Some(bounds) = doc.bounds() {
            let pad = 8.0;
            (
                bounds.min_x - pad,
                bounds.min_y - pad,
                bounds.max_x + pad,
                bounds.max_y + pad,
            )
        } else {
            let half = doc.field_mm / 2.0;
            (-half, -half, half, half)
        };
        self.center_x = (min_x + max_x) / 2.0;
        self.center_y = (min_y + max_y) / 2.0;
        let width = (max_x - min_x).max(1.0);
        let height = (max_y - min_y).max(1.0);
        self.zoom = (f64::from(size.x) / width).min(f64::from(size.y) / height);
    }

    fn to_screen(&self, x: f64, y: f64, origin: Pos2) -> Pos2 {
        Pos2::new(
            origin.x + ((x - self.center_x) * self.zoom) as f32,
            origin.y - ((y - self.center_y) * self.zoom) as f32,
        )
    }

    fn to_world(&self, screen: Pos2, origin: Pos2) -> [f64; 2] {
        [
            self.center_x + f64::from(screen.x - origin.x) / self.zoom,
            self.center_y - f64::from(screen.y - origin.y) / self.zoom,
        ]
    }
}

struct Studio {
    doc: Document,
    source: Option<PathBuf>,
    selected: Option<usize>,
    view: View,
    status: String,
    fit_next: bool,
}

impl Studio {
    fn new() -> Self {
        Self {
            doc: Document::new("Untitled"),
            source: None,
            selected: None,
            view: View {
                center_x: 0.0,
                center_y: 0.0,
                zoom: 6.0,
            },
            status: "Open a DXF or an EzCad .ezd file. Save writes .ezd or .dxf.".to_owned(),
            fit_next: true,
        }
    }

    fn open(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Drawings", &["dxf", "ezd", "DXF", "EZD"])
            .pick_file()
        else {
            return;
        };
        match open_drawing(&path) {
            Ok(doc) => {
                let paths = doc.paths.len();
                let points = doc.point_count();
                self.status = format!("Opened {} — {paths} paths, {points} points", doc.title);
                self.doc = doc;
                self.source = Some(path);
                self.selected = None;
                self.fit_next = true;
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn save(&mut self) {
        self.save_with("ezd", "EzCad");
    }

    fn save_dxf(&mut self) {
        self.save_with("dxf", "DXF");
    }

    fn save_with(&mut self, extension: &str, label: &str) {
        let start = self
            .source
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .map(|stem| format!("{stem}.{extension}"));
        let mut dialog = rfd::FileDialog::new().add_filter(label, &[extension]);
        if let Some(name) = start {
            dialog = dialog.set_file_name(name);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(extension);
        }
        let kind = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or(extension)
            .to_ascii_lowercase();
        let written = if kind == "dxf" {
            write_dxf(&path, &self.doc)
        } else {
            write_ezd(&path, &self.doc)
        };
        match written {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.source = Some(path);
            }
            Err(err) => self.status = err.to_string(),
        }
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::O)) {
            self.open();
        }
        if ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::S)) {
            self.save();
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("EZD Studio").strong().size(18.0));
                ui.add_space(12.0);
                if ui.button("Open").clicked() {
                    self.open();
                }
                if ui.button("Save .ezd").clicked() {
                    self.save();
                }
                if ui.button("Save .dxf").clicked() {
                    self.save_dxf();
                }
                if ui.button("Fit").clicked() {
                    self.fit_next = true;
                }
                if ui.button("Center on field").clicked() {
                    self.doc.center_on_field();
                    self.fit_next = true;
                    self.status = "Centered the drawing on the field origin".to_owned();
                }
                ui.separator();
                ui.label("Field");
                ui.add(
                    egui::DragValue::new(&mut self.doc.field_mm)
                        .range(20.0..=500.0)
                        .suffix(" mm")
                        .speed(1.0),
                );
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let bounds = self.doc.bounds();
                let size = bounds
                    .map(|bounds| format!("{:.1} × {:.1} mm", bounds.width(), bounds.height()))
                    .unwrap_or_else(|| "empty".to_owned());
                ui.label(format!(
                    "{}   ·   {} paths   ·   {size}",
                    self.doc.title,
                    self.doc.paths.len()
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status);
                });
            });
            ui.add_space(4.0);
        });

        egui::SidePanel::left("objects")
            .resizable(true)
            .default_width(280.0)
            .width_range(220.0..=420.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.heading("Paths");
                ui.label(
                    egui::RichText::new("DXF splines and lines become mark paths. Hatch fills are left out so the laser does not trace them twice.")
                        .small()
                        .weak(),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (index, path) in self.doc.paths.iter().enumerate() {
                        let selected = self.selected == Some(index);
                        let color = pen_color(&self.doc.pens, path.pen);
                        ui.horizontal(|ui| {
                            let (swatch, _) =
                                ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                            ui.painter().circle_filled(swatch.center(), 4.5, color);
                            if ui.selectable_label(selected, &path.name).clicked() {
                                self.selected = Some(index);
                            }
                        });
                    }
                    if !self.doc.notes.is_empty() {
                        ui.add_space(8.0);
                        ui.heading("Text in the file");
                        ui.label(
                            egui::RichText::new("These strings are stored in the .ezd. The letter outlines are already paths.")
                                .small()
                                .weak(),
                        );
                        for note in &self.doc.notes {
                            ui.label(note);
                        }
                    }
                });
            });

        egui::SidePanel::right("pens")
            .resizable(true)
            .default_width(300.0)
            .width_range(240.0..=420.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.heading("Pen");
                let pen_index = self
                    .selected
                    .and_then(|index| self.doc.paths.get(index))
                    .map(|path| path.pen)
                    .unwrap_or(0);
                ui.label(format!("Editing pen {pen_index}. Every path on this pen shares it."));
                if let Some(path_index) = self.selected {
                    ui.horizontal(|ui| {
                        ui.label("Assign path to");
                        let path = &mut self.doc.paths[path_index];
                        egui::ComboBox::from_id_salt("path-pen")
                            .selected_text(format!("Pen {}", path.pen))
                            .show_ui(ui, |ui| {
                                for index in 0..8 {
                                    ui.selectable_value(&mut path.pen, index, format!("Pen {index}"));
                                }
                            });
                    });
                }
                ui.add_space(6.0);
                if let Some(pen) = self.doc.pens.get_mut(pen_index) {
                    pen_editor(ui, pen);
                }
                ui.separator();
                ui.label("Palette");
                ui.horizontal_wrapped(|ui| {
                    for index in 0..8 {
                        let color = pen_color(&self.doc.pens, index);
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                        ui.painter().rect_filled(rect, 4.0, color);
                        if index == pen_index {
                            ui.painter().rect_stroke(
                                rect.expand(2.0),
                                4.0,
                                Stroke::new(1.5_f32, Color32::WHITE),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if response.clicked() {
                            if let Some(path_index) = self.selected {
                                self.doc.paths[path_index].pen = index;
                            }
                        }
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Speed is mm/s. Power is percent. Frequency is kHz. EzCad on Windows is where you confirm the file opens on the machine.")
                        .small()
                        .weak(),
                );
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let available = ui.available_rect_before_wrap();
            if self.fit_next && available.width() > 20.0 {
                self.view.fit(&self.doc, available.size());
                self.fit_next = false;
            }
            let response = ui.allocate_rect(available, Sense::click_and_drag());
            if response.dragged() {
                let delta = response.drag_delta();
                self.view.center_x -= f64::from(delta.x) / self.view.zoom;
                self.view.center_y += f64::from(delta.y) / self.view.zoom;
            }
            if response.hovered() {
                let scroll = ctx.input(|input| input.raw_scroll_delta.y);
                if scroll.abs() > 0.0 {
                    let pointer = ctx
                        .input(|input| input.pointer.hover_pos())
                        .unwrap_or(available.center());
                    let before = self.view.to_world(pointer, available.center());
                    let factor = (f64::from(scroll) * 0.0015).exp();
                    self.view.zoom = (self.view.zoom * factor).clamp(0.2, 80.0);
                    let after = self.view.to_world(pointer, available.center());
                    self.view.center_x += before[0] - after[0];
                    self.view.center_y += before[1] - after[1];
                }
            }
            paint_field(ui, available, &self.view, &self.doc, self.selected);
            if response.clicked() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    self.selected = pick_path(&self.doc, &self.view, available.center(), pointer);
                }
            }
        });
    }
}

fn pen_editor(ui: &mut egui::Ui, pen: &mut Pen) {
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut pen.name);
    });
    ui.horizontal(|ui| {
        ui.label("Color");
        let mut rgb = [pen.color[0], pen.color[1], pen.color[2]];
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            pen.color = rgb;
        }
    });
    labeled_drag(ui, "Speed", &mut pen.speed, 1.0, 5000.0, " mm/s");
    labeled_drag(ui, "Power", &mut pen.power, 0.0, 100.0, " %");
    labeled_drag(ui, "Frequency", &mut pen.frequency_khz, 1.0, 200.0, " kHz");
    ui.horizontal(|ui| {
        ui.label("Passes");
        ui.add(egui::DragValue::new(&mut pen.passes).range(1..=100).speed(0.05));
    });
}

fn labeled_drag(ui: &mut egui::Ui, label: &str, value: &mut f64, min: f64, max: f64, suffix: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::DragValue::new(value)
                .range(min..=max)
                .speed(0.5)
                .suffix(suffix),
        );
    });
}

fn pen_color(pens: &[Pen], index: usize) -> Color32 {
    pens.get(index).map_or(Color32::WHITE, |pen| {
        Color32::from_rgb(pen.color[0], pen.color[1], pen.color[2])
    })
}

fn paint_field(ui: &egui::Ui, rect: Rect, view: &View, doc: &Document, selected: Option<usize>) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(214, 210, 200));
    let origin = rect.center();
    let half = doc.field_mm / 2.0;
    let field = [
        view.to_screen(-half, -half, origin),
        view.to_screen(half, -half, origin),
        view.to_screen(half, half, origin),
        view.to_screen(-half, half, origin),
    ];
    painter.add(Shape::closed_line(
        field.to_vec(),
        Stroke::new(1.2_f32, Color32::from_rgb(120, 116, 108)),
    ));
    let grid = Color32::from_rgba_unmultiplied(120, 116, 108, 70);
    let mut tick = ((-half / 10.0).ceil()) * 10.0;
    while tick <= half {
        let a = view.to_screen(tick, -half, origin);
        let b = view.to_screen(tick, half, origin);
        painter.line_segment([a, b], Stroke::new(1.0_f32, grid));
        let c = view.to_screen(-half, tick, origin);
        let d = view.to_screen(half, tick, origin);
        painter.line_segment([c, d], Stroke::new(1.0_f32, grid));
        tick += 10.0;
    }
    let axis = Color32::from_rgb(90, 86, 78);
    painter.line_segment(
        [
            view.to_screen(-half, 0.0, origin),
            view.to_screen(half, 0.0, origin),
        ],
        Stroke::new(1.0_f32, axis),
    );
    painter.line_segment(
        [
            view.to_screen(0.0, -half, origin),
            view.to_screen(0.0, half, origin),
        ],
        Stroke::new(1.0_f32, axis),
    );

    for (index, path) in doc.paths.iter().enumerate() {
        let color = pen_color(&doc.pens, path.pen);
        let chosen = selected == Some(index);
        let stroke = if chosen {
            Stroke::new(2.4_f32, Color32::from_rgb(196, 112, 28))
        } else {
            Stroke::new(1.15_f32, color)
        };
        for contour in &path.contours {
            if contour.pts.len() < 2 {
                continue;
            }
            let points: Vec<Pos2> = contour
                .pts
                .iter()
                .map(|pt| view.to_screen(pt[0], pt[1], origin))
                .collect();
            if contour.closed {
                painter.add(Shape::closed_line(points, stroke));
            } else {
                painter.add(Shape::line(points, stroke));
            }
        }
    }
}

fn pick_path(doc: &Document, view: &View, origin: Pos2, pointer: Pos2) -> Option<usize> {
    let world = view.to_world(pointer, origin);
    let mut best: Option<(usize, f64)> = None;
    for (index, path) in doc.paths.iter().enumerate() {
        for contour in &path.contours {
            for window in contour.pts.windows(2) {
                let distance = segment_distance(world, window[0], window[1]);
                if distance < best.map_or(1.5, |(_, best_distance)| best_distance) {
                    best = Some((index, distance));
                }
            }
        }
    }
    best.map(|(index, _)| index)
}

fn segment_distance(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> f64 {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let len2 = dx * dx + dy * dy;
    let t = if len2 < 1e-12 {
        0.0
    } else {
        (((point[0] - start[0]) * dx + (point[1] - start[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    let x = start[0] + t * dx - point[0];
    let y = start[1] + t * dy - point[1];
    (x * x + y * y).sqrt()
}
