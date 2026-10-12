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
        self.end();
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
        self.end();
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
        self.end();
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
        self.end();
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
    /// are not an undo step.
    pub fn set_layer_props(&mut self, index: usize, props: LayerProps) -> Result<(), String> {
        self.end();
        let Some(layer) = self.doc.layer_mut(index) else {
            return Err("no such layer".to_string());
        };
        let previous = layer.props();
        if previous == props {
            return Ok(());
        }
        layer.name = props.name;
        layer.visible = props.visible;
        layer.opacity = props.opacity;
        self.history.record(Edit::Layer {
            name: "Layer Properties".to_string(),
            command: LayerCommand::Props {
                index,
                props: previous,
            },
        });
        self.layers_changed();
        Ok(())
    }

    fn layers_changed(&mut self) {
        self.preview = None;
        self.damage_all();
        self.bump();
    }
}
