//! A read-only snapshot of the codec for the HDMI bring-up (`SNDD:PROBE`):
//! what the codec says about itself, its output pin's presence and ELD, and
//! the converter's stream, format and digital settings. The Kaby Lake HDMI
//! codec is silent with a stream running and a valid ELD on the display side
//! (docs/kabylake-gpu-plan.md, G2), and none of this was observable; each
//! verb below is a plain get with no side effect.
//!
//! Verbs are HDA 1.0a section 7.3.3 (`GET_PIN_SENSE` `F09`, `GET_HDMI_ELD`
//! `F2F`, `GET_CONV_CHAN_COUNT` `F2D`, ...); they are named here, not in
//! `libs/hda`, until a driver needs one for more than a log line.

use hda::codec::{Graph, Path};
use hda::verbs::{param, Verbs};
use user::trace::{self, Channel};

const GET_POWER_STATE: u16 = 0xF05;
const GET_CONN_LIST: u16 = 0xF02;
const GET_STREAM_CHANNEL: u16 = 0xF06;
const GET_PIN_CONTROL: u16 = 0xF07;
const GET_PIN_SENSE: u16 = 0xF09;
const GET_DIGI_CONVERT: u16 = 0xF0D;
const GET_CHAN_COUNT: u16 = 0xF2D;
const GET_ELD_DATA: u16 = 0xF2F;
/// `GET_CONVERTER_FORMAT` is a 4-bit verb (`A`).
const GET_STREAM_FORMAT: u8 = 0xA;
/// Revision id (a parameter).
const PARAM_REVISION: u8 = 0x02;
/// Widget ids scanned for capabilities beyond what the group reported: the
/// Linux survey saw converters 2 to 4, pins 5 to 7 and a vendor widget at 8.
const SCAN: core::ops::RangeInclusive<u8> = 1..=12;
/// Parameter ids dumped per widget: vendor, revision, node count, function
/// type, caps, PCM sizes and rates, formats, pin caps, amp caps, connection
/// list length, power states, processing and volume caps.
const PARAMS: [u8; 13] = [
    0x00, 0x02, 0x04, 0x05, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x12,
];
/// ELD bytes dumped.
const ELD_BYTES: u8 = 24;

fn word(result: Result<u32, hda::verbs::VerbError>) -> alloc::string::String {
    match result {
        Ok(value) => alloc::format!("{value:#010x}"),
        Err(error) => alloc::format!("err:{error:?}"),
    }
}

/// One `SNDD:WIDGET` line per widget the group reported, before any path is
/// chosen: boot-1 reported pin 3 on the Kaby Lake HDMI codec, where Linux
/// sees pins 5 to 7 (docs/kabylake-gpu-plan.md 2.1), and only the real
/// numbering separates the explanations.
pub(super) fn widgets(graph: &Graph, codec: u8) {
    for widget in graph.widgets() {
        trace::line!(Channel::Sndd, "WIDGET codec={codec} {widget}");
    }
}

/// Log the snapshot. `when` says at which point of the driver's life it was
/// taken (`open`, `prepare`).
pub(super) fn dump(verbs: &mut impl Verbs, codec: u8, graph: &Graph, path: &Path, when: &str) {
    let line = |what: &str, text: alloc::string::String| {
        trace::line!(
            Channel::Sndd,
            "PROBE when={when} codec={codec} {what} {text}"
        );
    };
    line(
        "ids",
        alloc::format!(
            "vendor={} revision={} root_nodes={} afg_nodes={} afg_type={} afg_power={}",
            word(verbs.param(codec, 0, param::VENDOR_ID)),
            word(verbs.param(codec, 0, PARAM_REVISION)),
            word(verbs.param(codec, 0, param::NODE_COUNT)),
            word(verbs.param(codec, graph.afg, param::NODE_COUNT)),
            word(verbs.param(codec, graph.afg, param::FUNCTION_TYPE)),
            word(verbs.verb(codec, graph.afg, GET_POWER_STATE, 0)),
        ),
    );
    for nid in SCAN {
        let Ok(caps) = verbs.param(codec, nid, param::WIDGET_CAPS) else {
            continue;
        };
        if caps == 0 {
            continue;
        }
        let kind = (caps >> 20) & 0xF;
        let chosen = if nid == path.pin() || nid == path.dac() {
            "*"
        } else {
            ""
        };
        line(
            "widget",
            alloc::format!(
                "nid={nid}{chosen} type={kind} caps={caps:#010x} {}",
                detail(verbs, codec, nid, kind)
            ),
        );
        // The converter verbs on every widget, pins included: nid 3 reads as a
        // pin with converter 3 expected there, and what it answers to a
        // converter's verbs says which it is.
        line(
            "cvt-verbs",
            alloc::format!(
                "nid={nid} format={} stream={} digital={} channels={}",
                word(verbs.verb16(codec, nid, GET_STREAM_FORMAT, 0)),
                word(verbs.verb(codec, nid, GET_STREAM_CHANNEL, 0)),
                word(verbs.verb(codec, nid, GET_DIGI_CONVERT, 0)),
                word(verbs.verb(codec, nid, GET_CHAN_COUNT, 0)),
            ),
        );
        let mut params = alloc::string::String::new();
        for id in PARAMS {
            let value = word(verbs.param(codec, nid, id));
            params.push_str(&alloc::format!("p{id:02x}={value} "));
        }
        line("params", alloc::format!("nid={nid} {params}"));
    }
}

/// Pins: presence, control, connection list, ELD head. Converters: format,
/// stream, digital settings, channel count. Both: power state.
fn detail(verbs: &mut impl Verbs, codec: u8, nid: u8, kind: u32) -> alloc::string::String {
    let power = word(verbs.verb(codec, nid, GET_POWER_STATE, 0));
    match kind {
        4 => {
            let conns = word(verbs.param(codec, nid, param::CONN_LIST_LEN));
            let first = word(verbs.verb(codec, nid, GET_CONN_LIST, 0));
            let mut eld = alloc::string::String::new();
            for offset in 0..ELD_BYTES {
                match verbs.verb(codec, nid, GET_ELD_DATA, offset) {
                    Ok(v) => eld.push_str(&alloc::format!(
                        "{:02x}{} ",
                        v & 0xFF,
                        if v >> 31 != 0 { "" } else { "!" }
                    )),
                    Err(_) => eld.push_str("er "),
                }
            }
            alloc::format!(
                "sense={} control={} conn_len={conns} conn0={first} power={power} eld={eld}",
                word(verbs.verb(codec, nid, GET_PIN_SENSE, 0)),
                word(verbs.verb(codec, nid, GET_PIN_CONTROL, 0)),
            )
        }
        0 => alloc::format!(
            "format={} stream={} digital={} channels={} power={power}",
            word(verbs.verb16(codec, nid, GET_STREAM_FORMAT, 0)),
            word(verbs.verb(codec, nid, GET_STREAM_CHANNEL, 0)),
            word(verbs.verb(codec, nid, GET_DIGI_CONVERT, 0)),
            word(verbs.verb(codec, nid, GET_CHAN_COUNT, 0)),
        ),
        _ => alloc::format!("power={power}"),
    }
}
