#![forbid(unsafe_code)]

//! The layer commands, Layer Properties, the History jump and the image-wide
//! flips and turns: the app side of the Layers panel and its menus.

use xui_core::app::Ui;

use super::{Msg, PaintApp};
use crate::model::{Flip, LayerProps, Turn};
use crate::view::Command;

impl PaintApp {
    /// Runs the layer and image-orientation commands; `false` for any other.
    pub(super) fn layer_command(&mut self, ui: &Ui<Msg>, command: Command) -> bool {
        let active = self.model.document().active();
        let result = match command {
            Command::AddLayer => self.model.add_layer(),
            Command::DeleteLayer => self.model.delete_layer(active),
            Command::DuplicateLayer => self.model.duplicate_layer(),
            Command::MergeLayerDown => self.model.merge_layer_down(),
            Command::MoveLayerUp => self.model.move_layer(active, active + 1),
            Command::MoveLayerDown => self.model.move_layer(active, active.saturating_sub(1)),
            Command::FlipLayerHorizontal => self.model.flip_layer(Flip::Horizontal),
            Command::FlipLayerVertical => self.model.flip_layer(Flip::Vertical),
            Command::Flatten => self.model.flatten_image(),
            Command::FlipHorizontal => self.model.flip_image(Flip::Horizontal),
            Command::FlipVertical => self.model.flip_image(Flip::Vertical),
            Command::Rotate90Cw => self.model.rotate_image(Turn::Cw90),
            Command::Rotate90Ccw => self.model.rotate_image(Turn::Ccw90),
            Command::Rotate180 => self.model.rotate_image(Turn::Half),
            Command::LayerProperties => {
                self.open_props(ui, active);
                return true;
            }
            _ => return false,
        };
        self.message = Some(match result {
            Ok(()) => command.label().to_string(),
            Err(error) => format!("{} failed: {error}", command.label()),
        });
        true
    }

    /// A click on a layer row or its eye.
    pub(super) fn layer_msg(&mut self, ui: &Ui<Msg>, msg: Msg) {
        match msg {
            Msg::LayerSelect(index) => {
                self.model.select_layer(index);
            }
            Msg::LayerEye(index) => {
                if let Some(layer) = self.model.document().layer(index) {
                    let mut props = layer.props();
                    props.visible = !props.visible;
                    let _ = self.model.set_layer_props(index, props);
                }
            }
            Msg::LayerProps(index) if self.model.select_layer(index) => {
                self.open_props(ui, index);
            }
            _ => {}
        }
    }

    /// Opens Layer Properties on layer `index`, remembering what it had.
    fn open_props(&mut self, ui: &Ui<Msg>, index: usize) {
        let Some(layer) = self.model.document().layer(index) else {
            return;
        };
        let original = layer.props();
        self.props.open(ui, &original);
        self.canvas.set_visible(false);
        self.editing_layer = Some((index, original));
    }

    /// The dialog's controls changed: show them on the layer, without a step.
    pub(super) fn props_changed(&mut self) {
        let Some((index, _)) = self.editing_layer else {
            return;
        };
        let props = self.props.read();
        self.props.show_value(props.opacity);
        self.model.preview_layer_props(index, props);
    }

    /// Closes Layer Properties, keeping the edit as one step or restoring the
    /// layer.
    pub(super) fn close_props(&mut self, ui: &Ui<Msg>, keep: bool) {
        if let Some((index, original)) = self.editing_layer.take() {
            let result = if keep {
                let edited: LayerProps = self.props.read();
                self.model.apply_layer_props(index, original, edited)
            } else {
                self.model.preview_layer_props(index, original);
                Ok(())
            };
            if let Err(error) = result {
                self.message = Some(format!("Layer Properties failed: {error}"));
            }
        }
        self.props.close(ui);
        self.canvas.set_visible(true);
        ui.focus(self.canvas.id());
    }

    /// A History row was clicked: go to the state after `applied` edits.
    pub(super) fn history_jump(&mut self, applied: usize) {
        let before = self.model.history().undo_len();
        self.model.jump_to(applied);
        let now = self.model.history().undo_len();
        if now != before {
            self.message = Some(match self.model.history().undo_names().last() {
                Some(name) if now > 0 => format!("History: {name}"),
                _ => "History: start".to_string(),
            });
        }
    }
}
