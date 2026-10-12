#![forbid(unsafe_code)]

//! One undoable step.
//!
//! An [`Edit`] stores what is needed to reverse itself, and applying it swaps
//! that with the document's current state, so the same value is the redo after
//! an undo and the undo after a redo: no second copy of the pixels.

use crate::model::bitmap::Bitmap;
use crate::model::damage::Damage;
use crate::model::image::{Document, DocumentSnapshot, Layer, LayerProps};
use crate::model::rect::Rect;

/// A structural change to the layer stack. Applying one returns the command
/// that reverses it, so a stack entry flips between undo and redo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerCommand {
    /// Put `layer` at `index` and make `active` the active layer.
    Insert {
        /// Where the layer goes.
        index: usize,
        /// The layer (with its pixels).
        layer: Layer,
        /// The active layer afterwards.
        active: usize,
    },
    /// Take the layer at `index` out and make `active` the active layer.
    Remove {
        /// Which layer.
        index: usize,
        /// The active layer afterwards.
        active: usize,
    },
    /// Move the layer at `from` so it ends at `to`.
    Move {
        /// Where the layer is.
        from: usize,
        /// Where it ends.
        to: usize,
    },
    /// Replace the layer's name, visibility and opacity.
    Props {
        /// Which layer.
        index: usize,
        /// The properties to put in.
        props: LayerProps,
    },
}

impl LayerCommand {
    /// Applies the command and returns the one that undoes it.
    fn apply(self, doc: &mut Document) -> LayerCommand {
        match self {
            LayerCommand::Insert {
                index,
                layer,
                active,
            } => {
                let before = doc.active();
                doc.insert_layer(index, layer);
                doc.set_active(active);
                LayerCommand::Remove {
                    index,
                    active: before,
                }
            }
            LayerCommand::Remove { index, active } => {
                let before = doc.active();
                match doc.remove_layer(index) {
                    Some(layer) => {
                        doc.set_active(active);
                        LayerCommand::Insert {
                            index,
                            layer,
                            active: before,
                        }
                    }
                    // Refused (the last layer, or out of range): nothing moved.
                    None => LayerCommand::Remove { index, active },
                }
            }
            LayerCommand::Move { from, to } => {
                let active = doc.active();
                doc.move_layer_raw(from, to);
                doc.set_active(follow(active, from, to));
                LayerCommand::Move { from: to, to: from }
            }
            LayerCommand::Props { index, props } => {
                let Some(layer) = doc.layer_mut(index) else {
                    return LayerCommand::Props { index, props };
                };
                let previous = layer.props();
                layer.name = props.name;
                layer.visible = props.visible;
                layer.opacity = props.opacity;
                LayerCommand::Props {
                    index,
                    props: previous,
                }
            }
        }
    }

    fn bytes(&self) -> usize {
        match self {
            LayerCommand::Insert { layer, .. } => layer.pixels.byte_len(),
            _ => 0,
        }
    }
}

/// Where the `active` index lands when a layer moves from `from` to `to`.
pub(crate) fn follow(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && active <= to {
        active - 1
    } else if to <= active && active < from {
        active + 1
    } else {
        active
    }
}

/// One entry of the history, with the name the Edit menu and History panel
/// show ("Paintbrush", "Resize Image").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Pixels of one layer inside `rect` (the rectangle only, not the layer).
    /// `layer` is an index: edits are undone last-in first-out, so it is
    /// the layer's index at the time the edit is applied.
    Pixels {
        /// The display name.
        name: String,
        /// Which layer.
        layer: usize,
        /// The rectangle of the layer the edit touched.
        rect: Rect,
        /// The pixels of `rect` on the other side of the edit.
        before: Bitmap,
    },
    /// A change to the layer stack.
    Layer {
        /// The display name.
        name: String,
        /// What reverses it.
        command: LayerCommand,
    },
    /// The whole document (resize; later crop and rotate).
    Document {
        /// The display name.
        name: String,
        /// The document on the other side of the edit.
        before: Box<DocumentSnapshot>,
    },
}

impl Edit {
    /// The display name.
    pub fn name(&self) -> &str {
        match self {
            Edit::Pixels { name, .. } | Edit::Layer { name, .. } | Edit::Document { name, .. } => {
                name
            }
        }
    }

    /// The bytes the entry holds, which the history's cap counts.
    pub fn bytes(&self) -> usize {
        let held = match self {
            Edit::Pixels { before, .. } => before.byte_len(),
            Edit::Layer { command, .. } => command.bytes(),
            Edit::Document { before, .. } => before.bytes(),
        };
        held + self.name().len() + 64
    }

    /// Swaps the entry with the document's state and reports what changed.
    pub(crate) fn apply(&mut self, doc: &mut Document) -> Damage {
        match self {
            Edit::Pixels {
                layer,
                rect,
                before,
                ..
            } => {
                let swapped = doc
                    .layer_mut(*layer)
                    .is_some_and(|l| l.pixels.swap_region(rect.x0, rect.y0, before));
                if swapped {
                    Damage::Rect(*rect)
                } else {
                    Damage::None
                }
            }
            Edit::Layer { command, .. } => {
                // A placeholder only for the instant the command is consumed.
                let taken = std::mem::replace(command, LayerCommand::Move { from: 0, to: 0 });
                *command = taken.apply(doc);
                Damage::All
            }
            Edit::Document { before, .. } => {
                doc.swap_contents(before);
                Damage::All
            }
        }
    }
}
