// SPDX-License-Identifier: GPL-3.0-or-later

//! The in-player drawing overlay: a session-only tagged item list in
//! normalized video coordinates, rendered with Cairo/Pango on one
//! `GtkDrawingArea`. Tools are exactly the WR-000 exposed set: select/move,
//! freehand, line, arrow, rectangle, diamond, ellipse, text, eraser, stroke
//! color/width, undo/redo, and clear. Items clear on media change and are
//! never saved or exported.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

/// Bounded undo/redo depth for this session only.
const UNDO_LIMIT: usize = 64;
const HIT_RADIUS: f64 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Freehand,
    Line,
    Arrow,
    Rect,
    Diamond,
    Ellipse,
    Text,
    Eraser,
}

/// RGBA stroke color plus width in normalized units of the shorter video edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub rgba: (f64, f64, f64, f64),
    pub width: f64,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            rgba: (0.73, 0.26, 0.13, 1.0), // the app accent
            width: 0.004,
        }
    }
}

/// One drawn item. All coordinates are normalized to the video (0..1), so
/// items stay aligned through resize and fullscreen.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Freehand(Vec<(f64, f64)>),
    Line { a: (f64, f64), b: (f64, f64) },
    Arrow { a: (f64, f64), b: (f64, f64) },
    Rect { a: (f64, f64), b: (f64, f64) },
    Diamond { a: (f64, f64), b: (f64, f64) },
    Ellipse { a: (f64, f64), b: (f64, f64) },
    Text { at: (f64, f64), text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub shape: Shape,
    pub stroke: Stroke,
}

/// The GTK-free document: items plus a bounded undo/redo stack.
#[derive(Default)]
pub struct Doc {
    items: Vec<Item>,
    undo: Vec<Vec<Item>>,
    redo: Vec<Vec<Item>>,
}

impl Doc {
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Snapshot the current items before a mutation.
    fn checkpoint(&mut self) {
        self.undo.push(self.items.clone());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn push(&mut self, item: Item) {
        self.checkpoint();
        self.items.push(item);
    }

    pub fn remove_at(&mut self, point: (f64, f64)) -> bool {
        match hit_test(&self.items, point) {
            Some(index) => {
                self.checkpoint();
                self.items.remove(index);
                true
            }
            None => false,
        }
    }

    pub fn move_item(&mut self, index: usize, dx: f64, dy: f64) {
        if let Some(item) = self.items.get_mut(index) {
            translate(&mut item.shape, dx, dy);
        }
    }

    /// A move gesture checkpoints once when it starts, not per motion event.
    pub fn begin_move(&mut self) {
        self.checkpoint();
    }

    pub fn clear(&mut self) {
        if !self.items.is_empty() {
            self.checkpoint();
            self.items.clear();
        }
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.items, previous));
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.items, next));
        }
    }
}

/// Topmost item within `HIT_RADIUS` of the point, in normalized coordinates.
pub fn hit_test(items: &[Item], point: (f64, f64)) -> Option<usize> {
    items
        .iter()
        .rposition(|item| shape_distance(&item.shape, point) <= HIT_RADIUS + item.stroke.width)
}

fn shape_distance(shape: &Shape, point: (f64, f64)) -> f64 {
    match shape {
        Shape::Freehand(points) => points
            .windows(2)
            .map(|pair| segment_distance(pair[0], pair[1], point))
            .fold(f64::INFINITY, f64::min),
        Shape::Line { a, b } | Shape::Arrow { a, b } => segment_distance(*a, *b, point),
        Shape::Rect { .. } | Shape::Diamond { .. } => polygon(shape)
            .windows(2)
            .map(|pair| segment_distance(pair[0], pair[1], point))
            .fold(f64::INFINITY, f64::min),
        Shape::Ellipse { a, b } => {
            let (left, top, right, bottom) = corners(*a, *b);
            let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
            let (rx, ry) = (
                ((right - left) / 2.0).max(1e-6),
                ((bottom - top) / 2.0).max(1e-6),
            );
            // Approximate distance from the ellipse outline.
            let normalized = (((point.0 - cx) / rx).powi(2) + ((point.1 - cy) / ry).powi(2)).sqrt();
            (normalized - 1.0).abs() * rx.min(ry)
        }
        Shape::Text { at, .. } => {
            ((point.0 - at.0).powi(2) + (point.1 - at.1).powi(2)).sqrt() - 0.02
        }
    }
}

fn segment_distance(a: (f64, f64), b: (f64, f64), point: (f64, f64)) -> f64 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let length_squared = abx * abx + aby * aby;
    let t = if length_squared == 0.0 {
        0.0
    } else {
        (((point.0 - a.0) * abx + (point.1 - a.1) * aby) / length_squared).clamp(0.0, 1.0)
    };
    let (nx, ny) = (a.0 + t * abx, a.1 + t * aby);
    ((point.0 - nx).powi(2) + (point.1 - ny).powi(2)).sqrt()
}

fn corners(a: (f64, f64), b: (f64, f64)) -> (f64, f64, f64, f64) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

/// Closed outline (first point repeated last) for rectangle/diamond shapes.
fn polygon(shape: &Shape) -> Vec<(f64, f64)> {
    match shape {
        Shape::Rect { a, b } => {
            let (left, top, right, bottom) = corners(*a, *b);
            vec![
                (left, top),
                (right, top),
                (right, bottom),
                (left, bottom),
                (left, top),
            ]
        }
        Shape::Diamond { a, b } => {
            let (left, top, right, bottom) = corners(*a, *b);
            let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
            vec![(cx, top), (right, cy), (cx, bottom), (left, cy), (cx, top)]
        }
        _ => Vec::new(),
    }
}

fn translate(shape: &mut Shape, dx: f64, dy: f64) {
    let shift = |point: &mut (f64, f64)| {
        point.0 += dx;
        point.1 += dy;
    };
    match shape {
        Shape::Freehand(points) => points.iter_mut().for_each(shift),
        Shape::Line { a, b }
        | Shape::Arrow { a, b }
        | Shape::Rect { a, b }
        | Shape::Diamond { a, b }
        | Shape::Ellipse { a, b } => {
            shift(a);
            shift(b);
        }
        Shape::Text { at, .. } => shift(at),
    }
}

// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

pub struct Overlay {
    /// The transparent drawing surface stacked over the video.
    pub area: gtk4::DrawingArea,
    /// The tool row shown while drawing is enabled.
    pub toolbar: gtk4::Box,
    state: Rc<State>,
}

struct State {
    doc: RefCell<Doc>,
    tool: Cell<Tool>,
    stroke: Cell<Stroke>,
    /// The in-progress shape while the pointer is down.
    pending: RefCell<Option<Item>>,
    /// Selected item index and last pointer position during a move.
    moving: Cell<Option<(usize, (f64, f64))>>,
    enabled: Cell<bool>,
}

impl Overlay {
    pub fn new() -> Self {
        let area = gtk4::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_visible(false);
        let state = Rc::new(State {
            doc: RefCell::new(Doc::default()),
            tool: Cell::new(Tool::Freehand),
            stroke: Cell::new(Stroke::default()),
            pending: RefCell::new(None),
            moving: Cell::new(None),
            enabled: Cell::new(false),
        });
        let toolbar = build_toolbar(&state, &area);
        toolbar.set_visible(false);
        let overlay = Self {
            area,
            toolbar,
            state,
        };
        overlay.connect_draw();
        overlay.connect_pointer();
        overlay
    }

    /// Enabling shows the surface/tools; disabling keeps items (legacy: the
    /// toggle hides the canvas, items reset only on media change/remount).
    pub fn set_enabled(&self, enabled: bool) {
        self.state.enabled.set(enabled);
        self.area.set_visible(enabled);
        self.toolbar.set_visible(enabled);
    }

    /// Media change: drop all items and history.
    pub fn reset(&self) {
        *self.state.doc.borrow_mut() = Doc::default();
        self.area.queue_draw();
    }

    fn connect_draw(&self) {
        let state = Rc::clone(&self.state);
        self.area.set_draw_func(move |_, cr, width, height| {
            let scale = f64::from(width.min(height)).max(1.0);
            let to_px =
                |point: (f64, f64)| (point.0 * f64::from(width), point.1 * f64::from(height));
            let doc = state.doc.borrow();
            let pending = state.pending.borrow();
            for item in doc.items().iter().chain(pending.iter()) {
                let (r, g, b, a) = item.stroke.rgba;
                cr.set_source_rgba(r, g, b, a);
                cr.set_line_width((item.stroke.width * scale).max(1.0));
                draw_shape(cr, &item.shape, to_px, scale);
            }
        });
    }

    fn connect_pointer(&self) {
        let drag = gtk4::GestureDrag::new();
        let state = Rc::clone(&self.state);
        let area = self.area.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let point = normalize(&area, x, y);
            match state.tool.get() {
                Tool::Select => {
                    let index = hit_test(state.doc.borrow().items(), point);
                    if let Some(index) = index {
                        state.doc.borrow_mut().begin_move();
                        state.moving.set(Some((index, point)));
                    }
                }
                Tool::Eraser => {
                    if state.doc.borrow_mut().remove_at(point) {
                        area.queue_draw();
                    }
                }
                Tool::Text => {
                    prompt_text(&area, &state, point);
                }
                tool => {
                    let shape = match tool {
                        Tool::Freehand => Shape::Freehand(vec![point]),
                        Tool::Line => Shape::Line { a: point, b: point },
                        Tool::Arrow => Shape::Arrow { a: point, b: point },
                        Tool::Rect => Shape::Rect { a: point, b: point },
                        Tool::Diamond => Shape::Diamond { a: point, b: point },
                        Tool::Ellipse => Shape::Ellipse { a: point, b: point },
                        Tool::Select | Tool::Eraser | Tool::Text => unreachable!(),
                    };
                    *state.pending.borrow_mut() = Some(Item {
                        shape,
                        stroke: state.stroke.get(),
                    });
                    area.queue_draw();
                }
            }
        });
        let state = Rc::clone(&self.state);
        let area = self.area.clone();
        drag.connect_drag_update(move |gesture, dx, dy| {
            let Some((start_x, start_y)) = gesture.start_point() else {
                return;
            };
            let point = normalize(&area, start_x + dx, start_y + dy);
            if let Some((index, last)) = state.moving.get() {
                state
                    .doc
                    .borrow_mut()
                    .move_item(index, point.0 - last.0, point.1 - last.1);
                state.moving.set(Some((index, point)));
                area.queue_draw();
                return;
            }
            let mut pending = state.pending.borrow_mut();
            if let Some(item) = pending.as_mut() {
                match &mut item.shape {
                    Shape::Freehand(points) => points.push(point),
                    Shape::Line { b, .. }
                    | Shape::Arrow { b, .. }
                    | Shape::Rect { b, .. }
                    | Shape::Diamond { b, .. }
                    | Shape::Ellipse { b, .. } => *b = point,
                    Shape::Text { .. } => {}
                }
                drop(pending);
                area.queue_draw();
            }
        });
        let state = Rc::clone(&self.state);
        let area = self.area.clone();
        drag.connect_drag_end(move |_, _, _| {
            state.moving.set(None);
            if let Some(item) = state.pending.borrow_mut().take() {
                state.doc.borrow_mut().push(item);
            }
            area.queue_draw();
        });
        self.area.add_controller(drag);
    }
}

fn normalize(area: &gtk4::DrawingArea, x: f64, y: f64) -> (f64, f64) {
    let width = f64::from(area.width()).max(1.0);
    let height = f64::from(area.height()).max(1.0);
    ((x / width).clamp(0.0, 1.0), (y / height).clamp(0.0, 1.0))
}

fn draw_shape(
    cr: &gtk4::cairo::Context,
    shape: &Shape,
    to_px: impl Fn((f64, f64)) -> (f64, f64),
    scale: f64,
) {
    match shape {
        Shape::Freehand(points) => {
            let mut iter = points.iter();
            if let Some(first) = iter.next() {
                let (x, y) = to_px(*first);
                cr.move_to(x, y);
                for point in iter {
                    let (x, y) = to_px(*point);
                    cr.line_to(x, y);
                }
                let _ = cr.stroke();
            }
        }
        Shape::Line { a, b } => {
            let (ax, ay) = to_px(*a);
            let (bx, by) = to_px(*b);
            cr.move_to(ax, ay);
            cr.line_to(bx, by);
            let _ = cr.stroke();
        }
        Shape::Arrow { a, b } => {
            let (ax, ay) = to_px(*a);
            let (bx, by) = to_px(*b);
            cr.move_to(ax, ay);
            cr.line_to(bx, by);
            let _ = cr.stroke();
            let angle = (by - ay).atan2(bx - ax);
            let head = 0.02 * scale;
            for offset in [-0.5, 0.5] {
                cr.move_to(bx, by);
                cr.line_to(
                    bx - head * (angle + offset).cos(),
                    by - head * (angle + offset).sin(),
                );
                let _ = cr.stroke();
            }
        }
        Shape::Rect { .. } | Shape::Diamond { .. } => {
            for (index, point) in polygon(shape).into_iter().enumerate() {
                let (x, y) = to_px(point);
                if index == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            let _ = cr.stroke();
        }
        Shape::Ellipse { a, b } => {
            let (left, top, right, bottom) = corners(*a, *b);
            let (cx, cy) = to_px(((left + right) / 2.0, (top + bottom) / 2.0));
            let (x2, y2) = to_px((right, bottom));
            let (rx, ry) = ((x2 - cx).max(1.0), (y2 - cy).max(1.0));
            let _ = cr.save();
            cr.translate(cx, cy);
            cr.scale(rx, ry);
            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
            let _ = cr.restore();
            let _ = cr.stroke();
        }
        Shape::Text { at, text } => {
            let (x, y) = to_px(*at);
            cr.set_font_size(0.035 * scale);
            cr.move_to(x, y);
            let _ = cr.show_text(text);
        }
    }
}

/// A small modal prompt for the text tool.
fn prompt_text(area: &gtk4::DrawingArea, state: &Rc<State>, at: (f64, f64)) {
    let entry = gtk4::Entry::new();
    entry.set_activates_default(true);
    let dialog = adw::AlertDialog::new(Some("Add text"), None);
    dialog.set_extra_child(Some(&entry));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("add", "Add");
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");
    let state = Rc::clone(state);
    let parent = area.clone();
    let area = area.clone();
    dialog.connect_response(None, move |_, response| {
        let text = entry.text().trim().to_owned();
        if response == "add" && !text.is_empty() {
            state.doc.borrow_mut().push(Item {
                shape: Shape::Text { at, text },
                stroke: state.stroke.get(),
            });
            area.queue_draw();
        }
    });
    dialog.present(Some(&parent));
}

fn build_toolbar(state: &Rc<State>, area: &gtk4::DrawingArea) -> gtk4::Box {
    let toolbar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let controls = gtk4::FlowBox::new();
    controls.add_css_class("toolbar");
    controls.set_selection_mode(gtk4::SelectionMode::None);
    controls.set_homogeneous(false);
    controls.set_column_spacing(4);
    controls.set_row_spacing(4);
    controls.set_min_children_per_line(1);
    controls.set_hexpand(true);
    controls.set_halign(gtk4::Align::Fill);
    controls.set_focusable(false);
    toolbar.append(&controls);

    // The active tool stays visible while the complete labelled tool set lives
    // in one compact popover. This replaces a row of nine icon-only buttons
    // without changing the document/tool state.
    let tool_content = adw::ButtonContent::new();
    tool_content.set_icon_name("document-edit-symbolic");
    tool_content.set_label("Freehand");
    let tool_button = gtk4::MenuButton::new();
    tool_button.set_child(Some(&tool_content));
    tool_button.set_tooltip_text(Some("Choose drawing tool"));
    tool_button.update_property(&[
        gtk4::accessible::Property::Label("Freehand"),
        gtk4::accessible::Property::Description("Drawing tool"),
    ]);

    let tool_grid = gtk4::Grid::new();
    tool_grid.set_column_homogeneous(true);
    tool_grid.set_column_spacing(4);
    tool_grid.set_row_spacing(4);
    tool_grid.set_margin_top(8);
    tool_grid.set_margin_bottom(8);
    tool_grid.set_margin_start(8);
    tool_grid.set_margin_end(8);
    let tool_popover = gtk4::Popover::new();
    tool_popover.set_child(Some(&tool_grid));
    tool_button.set_popover(Some(&tool_popover));

    let tools: [(Tool, &str, &str); 9] = [
        (Tool::Select, "Select and move", "edit-find-symbolic"),
        (Tool::Freehand, "Freehand", "document-edit-symbolic"),
        (Tool::Line, "Line", "format-justify-fill-symbolic"),
        (Tool::Arrow, "Arrow", "go-next-symbolic"),
        (Tool::Rect, "Rectangle", "checkbox-symbolic"),
        (Tool::Diamond, "Diamond", "process-stop-symbolic"),
        (Tool::Ellipse, "Ellipse", "media-record-symbolic"),
        (Tool::Text, "Text", "insert-text-symbolic"),
        (Tool::Eraser, "Eraser", "edit-clear-symbolic"),
    ];
    let mut first_button: Option<gtk4::ToggleButton> = None;
    for (index, (tool, label, icon)) in tools.into_iter().enumerate() {
        let content = adw::ButtonContent::new();
        content.set_icon_name(icon);
        content.set_label(label);
        let button = gtk4::ToggleButton::new();
        button.set_child(Some(&content));
        button.add_css_class("flat");
        button.set_hexpand(true);
        button.set_tooltip_text(Some(label));
        button.update_property(&[gtk4::accessible::Property::Label(label)]);
        if let Some(first) = &first_button {
            button.set_group(Some(first));
        } else {
            first_button = Some(button.clone());
        }
        if tool == Tool::Freehand {
            button.set_active(true);
        }
        let state = Rc::clone(state);
        let selected_content = tool_content.clone();
        let selected_button = tool_button.clone();
        let popover = tool_popover.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                state.tool.set(tool);
                selected_content.set_icon_name(icon);
                selected_content.set_label(label);
                selected_button.update_property(&[gtk4::accessible::Property::Label(label)]);
                popover.popdown();
            }
        });
        tool_grid.attach(&button, (index % 2) as i32, (index / 2) as i32, 1, 1);
    }
    controls.insert(&tool_button, -1);

    // Stroke properties share a second named popover, keeping the color dialog
    // and numeric width fully reachable without imposing their natural widths
    // on the drawing toolbar.
    let stroke_button = gtk4::MenuButton::new();
    stroke_button.set_label("Stroke");
    stroke_button.set_tooltip_text(Some("Stroke color and width"));
    stroke_button.update_property(&[gtk4::accessible::Property::Label("Stroke color and width")]);
    let stroke_content = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    stroke_content.set_margin_top(8);
    stroke_content.set_margin_bottom(8);
    stroke_content.set_margin_start(8);
    stroke_content.set_margin_end(8);

    let color = gtk4::ColorDialogButton::new(Some(gtk4::ColorDialog::new()));
    let initial = state.stroke.get().rgba;
    color.set_rgba(&gtk4::gdk::RGBA::new(
        initial.0 as f32,
        initial.1 as f32,
        initial.2 as f32,
        initial.3 as f32,
    ));
    color.set_tooltip_text(Some("Stroke color"));
    color.update_property(&[gtk4::accessible::Property::Label("Stroke color")]);
    {
        let state = Rc::clone(state);
        color.connect_rgba_notify(move |button| {
            let rgba = button.rgba();
            let mut stroke = state.stroke.get();
            stroke.rgba = (
                f64::from(rgba.red()),
                f64::from(rgba.green()),
                f64::from(rgba.blue()),
                f64::from(rgba.alpha()),
            );
            state.stroke.set(stroke);
        });
    }
    let color_label = gtk4::Label::new(Some("Color"));
    color_label.set_hexpand(true);
    color_label.set_xalign(0.0);
    let color_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    color_row.append(&color_label);
    color_row.append(&color);
    stroke_content.append(&color_row);

    let width = gtk4::SpinButton::with_range(1.0, 16.0, 1.0);
    width.set_value(2.0);
    width.set_tooltip_text(Some("Stroke width"));
    width.update_property(&[gtk4::accessible::Property::Label("Stroke width")]);
    {
        let state = Rc::clone(state);
        width.connect_value_changed(move |spin| {
            let mut stroke = state.stroke.get();
            stroke.width = spin.value() * 0.002;
            state.stroke.set(stroke);
        });
    }
    let width_label = gtk4::Label::new(Some("Width"));
    width_label.set_hexpand(true);
    width_label.set_xalign(0.0);
    let width_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    width_row.append(&width_label);
    width_row.append(&width);
    stroke_content.append(&width_row);

    let stroke_popover = gtk4::Popover::new();
    stroke_popover.set_child(Some(&stroke_content));
    stroke_button.set_popover(Some(&stroke_popover));
    controls.insert(&stroke_button, -1);

    for (label, icon, action) in [
        ("Undo", "edit-undo-symbolic", 0),
        ("Redo", "edit-redo-symbolic", 1),
        ("Clear drawing", "user-trash-symbolic", 2),
    ] {
        let button = gtk4::Button::from_icon_name(icon);
        button.add_css_class("flat");
        button.set_tooltip_text(Some(label));
        button.update_property(&[gtk4::accessible::Property::Label(label)]);
        let state = Rc::clone(state);
        let area = area.clone();
        button.connect_clicked(move |_| {
            let mut doc = state.doc.borrow_mut();
            match action {
                0 => doc.undo(),
                1 => doc.redo(),
                _ => doc.clear(),
            }
            drop(doc);
            area.queue_draw();
        });
        controls.insert(&button, -1);
    }

    toolbar
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(a: (f64, f64), b: (f64, f64)) -> Item {
        Item {
            shape: Shape::Line { a, b },
            stroke: Stroke::default(),
        }
    }

    #[test]
    fn hit_testing_prefers_the_topmost_item_and_respects_radius() {
        let items = vec![
            line((0.1, 0.5), (0.9, 0.5)),
            line((0.1, 0.5), (0.9, 0.5)), // same place, drawn later
        ];
        assert_eq!(hit_test(&items, (0.5, 0.5)), Some(1));
        assert_eq!(hit_test(&items, (0.5, 0.8)), None);

        let rect = vec![Item {
            shape: Shape::Rect {
                a: (0.2, 0.2),
                b: (0.6, 0.6),
            },
            stroke: Stroke::default(),
        }];
        // The outline hits; the hollow center does not.
        assert_eq!(hit_test(&rect, (0.4, 0.2)), Some(0));
        assert_eq!(hit_test(&rect, (0.4, 0.4)), None);
    }

    #[test]
    fn undo_redo_and_clear_round_trip() {
        let mut doc = Doc::default();
        doc.push(line((0.0, 0.0), (1.0, 1.0)));
        doc.push(line((0.0, 1.0), (1.0, 0.0)));
        assert_eq!(doc.items().len(), 2);

        doc.undo();
        assert_eq!(doc.items().len(), 1);
        doc.redo();
        assert_eq!(doc.items().len(), 2);

        doc.clear();
        assert!(doc.items().is_empty());
        doc.undo();
        assert_eq!(doc.items().len(), 2);

        // A new edit clears the redo branch.
        doc.undo();
        doc.push(line((0.5, 0.5), (0.6, 0.6)));
        doc.redo();
        assert_eq!(doc.items().len(), 2);
    }

    #[test]
    fn moving_translates_every_point_and_erasing_removes_one_item() {
        let mut doc = Doc::default();
        doc.push(line((0.1, 0.1), (0.2, 0.2)));
        doc.begin_move();
        doc.move_item(0, 0.3, 0.4);
        match &doc.items()[0].shape {
            Shape::Line { a, b } => {
                assert!((a.0 - 0.4).abs() < 1e-9 && (a.1 - 0.5).abs() < 1e-9);
                assert!((b.0 - 0.5).abs() < 1e-9 && (b.1 - 0.6).abs() < 1e-9);
            }
            other => panic!("unexpected shape {other:?}"),
        }
        doc.undo();
        match &doc.items()[0].shape {
            Shape::Line { a, .. } => assert!((a.0 - 0.1).abs() < 1e-9),
            other => panic!("unexpected shape {other:?}"),
        }

        assert!(doc.remove_at((0.15, 0.15)));
        assert!(doc.items().is_empty());
        assert!(!doc.remove_at((0.15, 0.15)));
    }
}
