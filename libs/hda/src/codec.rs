//! Walking a codec's widget graph to find an output path: an output pin the
//! board wired to something (line out, then speaker, then headphone), back
//! through mixers and selectors to a digital-to-analog converter.
//!
//! The walk is generic (no per-codec quirk table): it reads the audio function
//! group's widgets, their connection lists and the pins' configuration
//! defaults, then searches breadth first from each candidate pin. Everything
//! the codec reports is bounded before use: node counts, list lengths and
//! connection ranges are capped, so a codec that lies costs a failed search,
//! never a loop.

use crate::verbs::GET_CONN_LIST;
use crate::verbs::{caps, config, param, pin, VerbError, Verbs, GET_CONFIG_DEFAULT};

/// Widgets one function group may have (node ids are 8 bits).
pub const MAX_WIDGETS: usize = 128;
/// Connections remembered per widget.
pub const MAX_CONNS: usize = 16;
/// Nodes in an output path: pin, up to three mixers/selectors, converter.
pub const MAX_PATH: usize = 5;

/// One widget of the audio function group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widget {
    pub nid: u8,
    pub caps: u32,
    pub conns: [u8; MAX_CONNS],
    pub conn_count: u8,
    /// Pins only: capabilities and configuration default.
    pub pin_caps: u32,
    pub config: u32,
}

impl Widget {
    pub fn kind(&self) -> u32 {
        (self.caps >> caps::TYPE_SHIFT) & 0xF
    }

    pub fn connections(&self) -> &[u8] {
        &self.conns[..usize::from(self.conn_count)]
    }

    fn device(&self) -> u32 {
        (self.config >> config::DEVICE_SHIFT) & 0xF
    }

    fn attached(&self) -> bool {
        self.config >> config::CONNECTIVITY_SHIFT != config::NO_CONNECTION
    }
}

/// The audio function group and its widgets.
pub struct Graph {
    pub afg: u8,
    pub widgets: [Option<Widget>; MAX_WIDGETS],
    pub count: usize,
}

impl Graph {
    pub fn widget(&self, nid: u8) -> Option<&Widget> {
        self.widgets[..self.count]
            .iter()
            .flatten()
            .find(|widget| widget.nid == nid)
    }
}

/// One hop of a path: the node and which of its inputs leads on (`None` for
/// the converter at the end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hop {
    pub nid: u8,
    pub input: Option<u8>,
}

/// An output path, pin first, converter last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Path {
    pub hops: [Hop; MAX_PATH],
    pub len: usize,
}

impl Path {
    pub fn hops(&self) -> &[Hop] {
        &self.hops[..self.len]
    }

    pub fn pin(&self) -> u8 {
        self.hops[0].nid
    }

    pub fn dac(&self) -> u8 {
        self.hops[self.len - 1].nid
    }
}

/// Why no output path was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    Verb(VerbError),
    /// The codec has no audio function group.
    NoAudioFunction,
    /// No attached output pin reaches a converter.
    NoPath,
}

impl From<VerbError> for CodecError {
    fn from(error: VerbError) -> CodecError {
        CodecError::Verb(error)
    }
}

/// `(start, count)` from a `NODE_COUNT` answer, the count capped.
fn children(answer: u32) -> (u8, usize) {
    let start = ((answer >> 16) & 0xFF) as u8;
    let count = (answer & 0xFF) as usize;
    (start, count.min(MAX_WIDGETS).min(256 - usize::from(start)))
}

/// Read `nid`'s connection list, expanding ranges, at most [`MAX_CONNS`].
fn connections(
    verbs: &mut impl Verbs,
    codec: u8,
    nid: u8,
) -> Result<([u8; MAX_CONNS], u8), VerbError> {
    let mut out = [0u8; MAX_CONNS];
    let mut count = 0usize;
    let length = verbs.param(codec, nid, param::CONN_LIST_LEN)?;
    let long = length & 0x80 != 0;
    let entries = (length & 0x7F) as usize;
    let (per_answer, width, range_bit) = if long { (2, 16, 0x8000) } else { (4, 8, 0x80) };
    let mut previous: Option<u16> = None;
    let mut index = 0;
    while index < entries && count < MAX_CONNS {
        let answer = verbs.verb(codec, nid, GET_CONN_LIST, index as u8)?;
        for slot in 0..per_answer {
            if index >= entries || count >= MAX_CONNS {
                break;
            }
            let raw = ((answer >> (slot * width)) & ((1 << width) - 1)) as u16;
            let node = raw & !range_bit;
            if raw & range_bit != 0 {
                // A range from the previous entry up to this one.
                let from = previous.map_or(node, |p| p.saturating_add(1));
                let mut next = from;
                while next <= node && count < MAX_CONNS {
                    out[count] = next as u8;
                    count += 1;
                    next += 1;
                }
            } else {
                out[count] = node as u8;
                count += 1;
            }
            previous = Some(node);
            index += 1;
        }
    }
    Ok((out, count as u8))
}

/// Read the codec's audio function group and its widgets.
pub fn read_graph(verbs: &mut impl Verbs, codec: u8) -> Result<Graph, CodecError> {
    let (start, count) = children(verbs.param(codec, 0, param::NODE_COUNT)?);
    let mut afg = None;
    for nid in start..start.saturating_add(count as u8) {
        if verbs.param(codec, nid, param::FUNCTION_TYPE)? & 0xFF == 1 {
            afg = Some(nid);
            break;
        }
    }
    let afg = afg.ok_or(CodecError::NoAudioFunction)?;
    let (first, count) = children(verbs.param(codec, afg, param::NODE_COUNT)?);
    let mut graph = Graph {
        afg,
        widgets: [None; MAX_WIDGETS],
        count: 0,
    };
    for offset in 0..count {
        let nid = first.wrapping_add(offset as u8);
        let caps = verbs.param(codec, nid, param::WIDGET_CAPS)?;
        let (conns, conn_count) = if caps & caps::CONN_LIST != 0 {
            connections(verbs, codec, nid)?
        } else {
            ([0; MAX_CONNS], 0)
        };
        let is_pin = (caps >> caps::TYPE_SHIFT) & 0xF == caps::PIN;
        let (pin_caps, config) = if is_pin {
            (
                verbs.param(codec, nid, param::PIN_CAPS)?,
                verbs.verb(codec, nid, GET_CONFIG_DEFAULT, 0)?,
            )
        } else {
            (0, 0)
        };
        graph.widgets[graph.count] = Some(Widget {
            nid,
            caps,
            conns,
            conn_count,
            pin_caps,
            config,
        });
        graph.count += 1;
    }
    Ok(graph)
}

/// The output pins in order of preference: attached, output capable, line out
/// before speaker before headphone before anything else.
fn candidate_pins(graph: &Graph) -> impl Iterator<Item = &Widget> {
    let rank = |widget: &Widget| match widget.device() {
        config::LINE_OUT => 0,
        config::SPEAKER => 1,
        config::HEADPHONE => 2,
        _ => 3,
    };
    (0..4).flat_map(move |wanted| {
        graph.widgets[..graph.count]
            .iter()
            .flatten()
            .filter(move |widget| {
                widget.kind() == caps::PIN
                    && widget.pin_caps & pin::CAP_OUTPUT != 0
                    && widget.attached()
                    && rank(widget) == wanted
            })
    })
}

/// Breadth-first from `pin` to the nearest converter through mixers and
/// selectors.
fn search(graph: &Graph, pin: &Widget) -> Option<Path> {
    // Each queue entry is a partial path; the graph is small and paths short.
    let mut queue: [Option<Path>; 64] = [None; 64];
    let (mut head, mut tail) = (0, 0);
    let start = Path {
        hops: [Hop {
            nid: pin.nid,
            input: None,
        }; MAX_PATH],
        len: 1,
    };
    queue[tail] = Some(start);
    tail += 1;
    while head < tail {
        let path = queue[head].take()?;
        head += 1;
        let last = graph.widget(path.hops[path.len - 1].nid)?;
        for (index, &next) in last.connections().iter().enumerate() {
            let Some(widget) = graph.widget(next) else {
                continue;
            };
            if path.hops().iter().any(|hop| hop.nid == next) {
                continue;
            }
            let mut longer = path;
            longer.hops[longer.len - 1].input = Some(index as u8);
            longer.hops[longer.len] = Hop {
                nid: next,
                input: None,
            };
            longer.len += 1;
            match widget.kind() {
                caps::OUTPUT => return Some(longer),
                caps::MIXER | caps::SELECTOR if longer.len < MAX_PATH && tail < queue.len() => {
                    queue[tail] = Some(longer);
                    tail += 1;
                }
                _ => {}
            }
        }
    }
    None
}

/// The best output path the codec offers.
pub fn find_output(graph: &Graph) -> Result<Path, CodecError> {
    candidate_pins(graph)
        .find_map(|pin| search(graph, pin))
        .ok_or(CodecError::NoPath)
}
