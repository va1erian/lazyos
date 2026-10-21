//! An HDMI display codec on the HDA link (docs/kabylake-gpu-plan.md, G2):
//! `hda::hdmi` picks the pin that senses a monitor and drives the converters
//! and the Audio InfoFrame; any other codec keeps the generic walk. This is
//! the glue `HdaCard` calls, so the card itself stays one code path.

use hda::codec::{find_output, Graph, Path};
use hda::hdmi::{self, Hdmi};
use hda::verbs::Verbs;
use user::trace::{self, Channel};

use crate::error::Error;

/// How often the pins are asked for a sink before the codec is taken for one
/// without a monitor (each try waits one `nap`).
const SENSE_TRIES: u32 = 20;

fn failed(what: &'static str) -> impl Fn(hda::verbs::VerbError) -> Error {
    move |error| {
        trace::line!(Channel::Sndd, "HDA:ERR {what}: {error:?}");
        Error::Hda(what)
    }
}

/// The output path: the HDMI pin with a sink when the codec has one, else the
/// generic best output.
pub(super) fn choose(
    verbs: &mut impl Verbs,
    codec: u8,
    graph: &Graph,
) -> Result<(Path, Option<Hdmi>), Error> {
    hdmi::power_up(verbs, codec, graph).map_err(failed("hdmi power"))?;
    // A pin reports its sink a little after the group wakes.
    let mut sensed = None;
    for _ in 0..SENSE_TRIES {
        sensed = hdmi::find(verbs, codec, graph).map_err(failed("hdmi sense"))?;
        if sensed.is_some() {
            break;
        }
        user::sys::nap();
    }
    if let Some(found) = sensed {
        trace::line!(
            Channel::Sndd,
            "HDMI pin={} eld_valid={} converters={:?}",
            found.pin,
            u8::from(found.eld_valid),
            found.converters()
        );
        return Ok((hdmi::path(&found), Some(found)));
    }
    let path = find_output(graph).map_err(|error| {
        trace::line!(Channel::Sndd, "HDA:ERR output path: {error:?}");
        Error::Hda("output path")
    })?;
    Ok((path, None))
}

/// Power the HDMI pin and every converter it may use (a no-op otherwise).
pub(super) fn enable(verbs: &mut impl Verbs, codec: u8, hdmi: Option<&Hdmi>) -> Result<(), Error> {
    match hdmi {
        Some(hdmi) => hdmi::enable(verbs, codec, hdmi).map_err(failed("hdmi enable")),
        None => Ok(()),
    }
}

/// Bind every HDMI converter to stream `tag` and send the Audio InfoFrame
/// (a no-op otherwise); `tag` 0 unbinds.
pub(super) fn bind(
    verbs: &mut impl Verbs,
    codec: u8,
    hdmi: Option<&Hdmi>,
    tag: u8,
    format: u16,
    channels: u8,
) -> Result<(), Error> {
    match hdmi {
        Some(hdmi) => {
            hdmi::bind(verbs, codec, hdmi, tag, format, channels).map_err(failed("hdmi bind"))
        }
        None => Ok(()),
    }
}
