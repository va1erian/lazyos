#![forbid(unsafe_code)]

//! Layer actions on the model: the API the Layers panel and menu will call.
//! Each (except selecting) is one undo step.

use super::Model;
use crate::model::history::follow;
use crate::model::{Bitmap, Edit, Layer, LayerCommand, LayerProps};

impl Model {
    /// Makes layer `index` the one tools draw on; `false` when out of range.
    /// Selecting is not an undo step.
    pub fn select_layer(&mut self, index: usize) -> bool {
        if index >= self.doc.layers().len() {
            return false;
        }
        self.settle();
        self.doc.set_active(index);
        self.bump();
        true
    }

    /// Adds a transparent layer above the active one and selects it. Refused
    /// when the extra layer would exceed the memory budget.
    pub fn add_layer(&mut self) -> Result<(), String> {
        let (width, height) = self.doc.size();
        self.add_layer_with(Layer::new(
            format!("Layer {}", self.doc.layers().len() + 1),
            Bitmap::transparent(width, height),
        ))
    }

    /// Adds a copy of the active layer above it, named "<name> copy".
    pub fn duplicate_layer(&mut self) -> Result<(), String> {
        self.check_fits(
            self.doc.width(),
            self.doc.height(),
            self.doc.layers().len() + 1,
        )?;
        let mut copy = self.doc.active_layer().clone();
        copy.name.push_str(" copy");
        self.add_layer_with(copy)
    }

    /// Inserts `layer` (which must match the document's size) above the active
    /// layer after the budget check.
    fn add_layer_with(&mut self, layer: Layer) -> Result<(), String> {
        self.settle();
        self.check_fits(
            self.doc.width(),
            self.doc.height(),
            self.doc.layers().len() + 1,
        )?;
        if layer.pixels.size() != self.doc.size() {
            return Err("the layer does not match the image size".to_string());
        }
        let index = self.doc.active() + 1;
        let previous = self.doc.active();
        self.doc.insert_layer(index, layer);
        self.doc.set_active(index);
        self.history.record(Edit::Layer {
            name: "Add Layer".to_string(),
            command: LayerCommand::Remove {
                index,
                active: previous,
            },
        });
        self.layers_changed();
        Ok(())
    }

    /// Deletes layer `index`; the last layer cannot be deleted.
    pub fn delete_layer(&mut self, index: usize) -> Result<(), String> {
        self.settle();
        if self.doc.layers().len() <= 1 {
            return Err("an image needs at least one layer".to_string());
        }
        let previous = self.doc.active();
        let Some(layer) = self.doc.remove_layer(index) else {
            return Err("no such layer".to_string());
        };
        // Select the layer below, as the removed one's neighbour.
        let active = if previous > index {
            previous - 1
        } else if previous == index {
            index.saturating_sub(1)
        } else {
            previous
        };
        self.doc.set_active(active);
        self.history.record(Edit::Layer {
            name: "Delete Layer".to_string(),
            command: LayerCommand::Insert {
                index,
                layer,
                active: previous,
            },
        });
        self.layers_changed();
        Ok(())
    }

    /// Moves layer `from` so it ends at index `to` (0 is the bottom).
    pub fn move_layer(&mut self, from: usize, to: usize) -> Result<(), String> {
        self.settle();
        let count = self.doc.layers().len();
        if from >= count || to >= count {
            return Err("no such layer".to_string());
        }
        if from == to {
            return Ok(());
        }
        let active = self.doc.active();
        self.doc.move_layer_raw(from, to);
        self.doc.set_active(follow(active, from, to));
        self.history.record(Edit::Layer {
            name: "Move Layer".to_string(),
            command: LayerCommand::Move { from: to, to: from },
        });
        self.layers_changed();
        Ok(())
    }

    /// Sets layer `index`'s name, visibility and opacity; unchanged properties
    /// are not an undo step. The step is named for what changed ("Hide
    /// Layer", "Layer Opacity", "Rename Layer"), else "Layer Properties".
    pub fn set_layer_props(&mut self, index: usize, props: LayerProps) -> Result<(), String> {
        self.settle();
        let Some(layer) = self.doc.layer_mut(index) else {
            return Err("no such layer".to_string());
        };
        let previous = layer.props();
        if previous == props {
            return Ok(());
        }
        let name = props_step_name(&previous, &props);
        layer.name = props.name;
        layer.visible = props.visible;
        layer.opacity = props.opacity;
        self.history.record(Edit::Layer {
            name: name.to_string(),
            command: LayerCommand::Props {
                index,
                props: previous,
            },
        });
        self.layers_changed();
        Ok(())
    }

    /// Shows `props` on layer `index` without a history step: the live
    /// preview of Layer Properties. Finish with [`Model::apply_layer_props`]
    /// (OK) or another preview of the original (Cancel).
    pub fn preview_layer_props(&mut self, index: usize, props: LayerProps) {
        self.settle();
        if let Some(layer) = self.doc.layer_mut(index)
            && layer.props() != props
        {
            layer.name = props.name;
            layer.visible = props.visible;
            layer.opacity = props.opacity;
            self.layers_changed();
        }
    }

    /// Commits a previewed edit as one step: `original` is what the layer had
    /// before the preview began, `props` what it shows now.
    pub fn apply_layer_props(
        &mut self,
        index: usize,
        original: LayerProps,
        props: LayerProps,
    ) -> Result<(), String> {
        self.preview_layer_props(index, original);
        self.set_layer_props(index, props)
    }

    fn layers_changed(&mut self) {
        self.preview = None;
        self.damage_all();
        self.bump();
    }
}

/// The history name for a property change.
fn props_step_name(before: &LayerProps, after: &LayerProps) -> &'static str {
    let name = before.name != after.name;
    let shown = before.visible != after.visible;
    let opacity = before.opacity != after.opacity;
    match (name, shown, opacity) {
        (true, false, false) => "Rename Layer",
        (false, true, false) if after.visible => "Show Layer",
        (false, true, false) => "Hide Layer",
        (false, false, true) => "Layer Opacity",
        _ => "Layer Properties",
    }
}
