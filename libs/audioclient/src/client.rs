//! Typed calls: one method per IDL method, encoded with the generated codecs.

use alloc::vec::Vec;

use crate::{
    control_wire as control, wire, Error, Grant, Info, Result, RingRef, Transfers, Transport,
};

fn malformed<E>(_: E) -> Error {
    Error::Malformed
}

/// A client of `os.lazy.audio.v1`, against the mixer or a card.
pub struct Client<T> {
    transport: T,
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Client<T> {
        Client { transport }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn call(&self, method: u32, body: Vec<u8>) -> Result<Vec<u8>> {
        self.transport
            .call(wire::INTERFACE_ID, method, body, Transfers::NONE, None)
    }

    /// Send an arbitrary request (hostile-input tests): `body` and
    /// `transfers` go out as given, declared or not.
    pub fn raw(&self, method: u32, body: Vec<u8>, transfers: Transfers) -> Result<Vec<u8>> {
        self.transport
            .call(wire::INTERFACE_ID, method, body, transfers, None)
    }

    /// `Info()`.
    pub fn info(&self) -> Result<Info> {
        let reply = self.call(wire::METHOD_INFO, Vec::new())?;
        Ok(wire::decode_info_reply(&reply).map_err(malformed)?.info)
    }

    /// `OpenStream(...)`: the service grants the closest parameters it can.
    pub fn open_stream(
        &self,
        dir: u32,
        format: u32,
        rate: u32,
        channels: u32,
        period_bytes: u32,
    ) -> Result<Grant> {
        let body = wire::encode_open_stream_args(&wire::OpenStreamArgs {
            dir,
            format,
            rate,
            channels,
            period_bytes,
        })
        .map_err(malformed)?;
        let reply = self.call(wire::METHOD_OPENSTREAM, body)?;
        Ok(wire::decode_open_stream_reply(&reply)
            .map_err(malformed)?
            .grant)
    }

    /// `AttachRing(stream)` with `ring` as the request's `Ring<Samples>`.
    pub fn attach_ring(&self, stream: u32, ring: RingRef) -> Result<()> {
        let body =
            wire::encode_attach_ring_args(&wire::AttachRingArgs { stream }).map_err(malformed)?;
        let transfers =
            wire::encode_attach_ring_transfers(&wire::AttachRingTransfers { ring: ring.desc() });
        self.raw(wire::METHOD_ATTACHRING, body, transfers.into())
            .map(|_| ())
    }

    /// `Commit(stream, written)`: returns the frames consumed so far.
    pub fn commit(&self, stream: u32, written_frames: u64) -> Result<u64> {
        let body = wire::encode_commit_args(&wire::CommitArgs {
            stream,
            written_frames,
        })
        .map_err(malformed)?;
        let reply = self.call(wire::METHOD_COMMIT, body)?;
        Ok(wire::decode_commit_reply(&reply)
            .map_err(malformed)?
            .consumed)
    }

    pub fn start(&self, stream: u32) -> Result<()> {
        let body = wire::encode_start_args(&wire::StartArgs { stream }).map_err(malformed)?;
        self.call(wire::METHOD_START, body).map(|_| ())
    }

    pub fn stop(&self, stream: u32) -> Result<()> {
        let body = wire::encode_stop_args(&wire::StopArgs { stream }).map_err(malformed)?;
        self.call(wire::METHOD_STOP, body).map(|_| ())
    }

    /// `Drain(stream)`: returns once everything committed has played, or
    /// fails at `deadline` (an absolute tick; `None` waits as long as it
    /// takes).
    pub fn drain(&self, stream: u32, deadline: Option<u64>) -> Result<()> {
        let body = wire::encode_drain_args(&wire::DrainArgs { stream }).map_err(malformed)?;
        self.transport
            .call(
                wire::INTERFACE_ID,
                wire::METHOD_DRAIN,
                body,
                Transfers::NONE,
                deadline,
            )
            .map(|_| ())
    }

    /// `Position(stream)`: frames played since the last start.
    pub fn position(&self, stream: u32) -> Result<u64> {
        let body = wire::encode_position_args(&wire::PositionArgs { stream }).map_err(malformed)?;
        let reply = self.call(wire::METHOD_POSITION, body)?;
        Ok(wire::decode_position_reply(&reply)
            .map_err(malformed)?
            .frames)
    }

    pub fn close_stream(&self, stream: u32) -> Result<()> {
        let body =
            wire::encode_close_stream_args(&wire::CloseStreamArgs { stream }).map_err(malformed)?;
        self.call(wire::METHOD_CLOSESTREAM, body).map(|_| ())
    }

    /// `SetVolume(stream, gain_q16)`: 65536 is unity.
    pub fn set_volume(&self, stream: u32, gain_q16: u32) -> Result<()> {
        let body = wire::encode_set_volume_args(&wire::SetVolumeArgs { stream, gain_q16 })
            .map_err(malformed)?;
        self.call(wire::METHOD_SETVOLUME, body).map(|_| ())
    }

    pub fn set_mute(&self, stream: u32, mute: bool) -> Result<()> {
        let body =
            wire::encode_set_mute_args(&wire::SetMuteArgs { stream, mute }).map_err(malformed)?;
        self.call(wire::METHOD_SETMUTE, body).map(|_| ())
    }
}

/// A client of `os.lazy.audio.mixer.v1`: the volume control panel.
pub struct MixerControl<T> {
    transport: T,
}

impl<T: Transport> MixerControl<T> {
    pub fn new(transport: T) -> MixerControl<T> {
        MixerControl { transport }
    }

    fn call(&self, method: u32, body: Vec<u8>) -> Result<Vec<u8>> {
        self.transport
            .call(control::INTERFACE_ID, method, body, Transfers::NONE, None)
    }

    /// Every open stream.
    pub fn streams(&self) -> Result<Vec<control::StreamStatus>> {
        let reply = self.call(control::METHOD_LISTSTREAMS, Vec::new())?;
        Ok(control::decode_list_streams_reply(&reply)
            .map_err(malformed)?
            .streams)
    }

    pub fn set_stream_volume(&self, stream: u32, gain_q16: u32, mute: bool) -> Result<()> {
        let body = control::encode_set_stream_volume_args(&control::SetStreamVolumeArgs {
            stream,
            gain_q16,
            mute,
        })
        .map_err(malformed)?;
        self.call(control::METHOD_SETSTREAMVOLUME, body).map(|_| ())
    }

    pub fn master(&self) -> Result<control::Master> {
        let reply = self.call(control::METHOD_GETMASTER, Vec::new())?;
        Ok(control::decode_get_master_reply(&reply)
            .map_err(malformed)?
            .master)
    }

    pub fn set_master(&self, gain_q16: u32, mute: bool) -> Result<()> {
        let body = control::encode_set_master_args(&control::SetMasterArgs { gain_q16, mute })
            .map_err(malformed)?;
        self.call(control::METHOD_SETMASTER, body).map(|_| ())
    }
}
