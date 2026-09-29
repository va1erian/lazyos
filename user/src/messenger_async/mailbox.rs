use libmessenger::{flags, Encoder, Header, VERSION};

use crate::messenger::{Endpoint, Error, Message, Result};

use super::{
    error_parcel, Parcel, CODE_HANDLER, CODE_UNKNOWN_INTERFACE, FIELD_HEARTBEATS, FIELD_SERVED,
    FIELD_SHUTTING_DOWN,
};

// ---------------------------------------------------------------------------
// `service!` runtime
// ---------------------------------------------------------------------------

/// The runtime half of [`service!`](crate::service): a single-threaded mailbox that dispatches
/// declared methods, answers and emits heartbeats, and honours a graceful
/// shutdown method.
///
/// `Mailbox` is meant to be embedded by the macro, but it is public so a
/// hand-written dispatcher can reuse the same health/shutdown plumbing.
pub struct Mailbox {
    endpoint: Endpoint,
    interface_id: u64,
    heartbeat_method: u32,
    heartbeat_every: u64,
    heartbeat_out: Option<Endpoint>,
    shutdown_method: u32,
    served: u64,
    since_heartbeat: u64,
    heartbeats: u64,
    shutting_down: bool,
}

impl Mailbox {
    /// Wrap the endpoint the service receives on. Method id `0` disables the
    /// heartbeat or shutdown clause (`every: 0` disables periodic emission).
    pub const fn new(
        endpoint: Endpoint,
        interface_id: u64,
        heartbeat_method: u32,
        heartbeat_every: u64,
        shutdown_method: u32,
    ) -> Mailbox {
        Mailbox {
            endpoint,
            interface_id,
            heartbeat_method,
            heartbeat_every,
            heartbeat_out: None,
            shutdown_method,
            served: 0,
            since_heartbeat: 0,
            heartbeats: 0,
            shutting_down: false,
        }
    }

    /// The endpoint this mailbox receives on.
    pub const fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// The endpoint future heartbeats are sent on; its peer is the observer.
    pub fn set_heartbeat_endpoint(&mut self, endpoint: Option<Endpoint>) {
        self.heartbeat_out = endpoint;
    }

    /// Messages dispatched so far.
    pub const fn served(&self) -> u64 {
        self.served
    }

    /// Whether a graceful shutdown was requested.
    pub const fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// Dispatch one message through `handler` for ordinary methods.
    ///
    /// Returns `Ok(false)` after the shutdown method ran, so [`Mailbox::run`]
    /// can stop. Interface mismatches and unknown methods are answered with an
    /// error parcel instead of dropped, so a synchronous caller never hangs.
    pub fn dispatch(
        &mut self,
        message: &Message,
        handler: &mut dyn FnMut(&Message) -> Result<Parcel>,
    ) -> Result<bool> {
        self.served = self.served.saturating_add(1);
        self.since_heartbeat = self.since_heartbeat.saturating_add(1);

        if message.interface_id() != self.interface_id {
            let reply = error_parcel(
                self.interface_id,
                message.method(),
                CODE_UNKNOWN_INTERFACE,
                "unknown interface",
            );
            self.respond(message, reply)?;
            return Ok(true);
        }

        if self.heartbeat_method != 0 && message.method() == self.heartbeat_method {
            let reply = self.health_parcel(flags::SYNC, false)?;
            self.respond(message, Ok(reply))?;
        } else if self.shutdown_method != 0 && message.method() == self.shutdown_method {
            let reply = self.health_parcel(flags::SYNC, true)?;
            self.respond(message, Ok(reply))?;
            self.shutting_down = true;
            // Retained-ish: the last heartbeat an observer sees carries the
            // shutdown flag, so a controller does not need a separate goodbye.
            self.emit_heartbeat(true)?;
            return Ok(false);
        } else {
            let reply = handler(message);
            self.respond(message, reply)?;
        }

        if self.heartbeat_every != 0 && self.since_heartbeat >= self.heartbeat_every {
            self.emit_heartbeat(false)?;
        }
        Ok(true)
    }

    /// Receive one message and dispatch it. Blocks in `recv` when idle.
    pub fn serve_once(
        &mut self,
        handler: &mut dyn FnMut(&Message) -> Result<Parcel>,
    ) -> Result<bool> {
        let message = self.endpoint.recv(None)?;
        self.dispatch(&message, handler)
    }

    /// [`Mailbox::serve_once`] until the shutdown method runs.
    pub fn run(&mut self, handler: &mut dyn FnMut(&Message) -> Result<Parcel>) -> Result<()> {
        while self.serve_once(handler)? {}
        Ok(())
    }

    /// Send one heartbeat to the observer now; a no-op without an observer.
    pub fn heartbeat(&mut self) -> Result<()> {
        self.emit_heartbeat(false)
    }

    /// Build the health parcel: served count, emitted-heartbeat count, and the
    /// shutdown flag. Clients decode the [`FIELD_SERVED`] and
    /// [`FIELD_SHUTTING_DOWN`] fields (see the `async_service` example).
    pub fn health_parcel(&self, flags: u16, shutting_down: bool) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(FIELD_SERVED, self.served).map_err(Error::Parcel)?;
        body.u64(FIELD_HEARTBEATS, self.heartbeats)
            .map_err(Error::Parcel)?;
        body.bool(FIELD_SHUTTING_DOWN, shutting_down)
            .map_err(Error::Parcel)?;
        Ok(Parcel {
            header: Header {
                version: VERSION,
                flags,
                interface_id: self.interface_id,
                method: self.heartbeat_method,
                ..Header::default()
            },
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Send a heartbeat on the observer endpoint and bump the counters.
    fn emit_heartbeat(&mut self, shutting_down: bool) -> Result<()> {
        let Some(out) = self.heartbeat_out else {
            return Ok(());
        };
        let parcel = self.health_parcel(flags::ONE_WAY, shutting_down)?;
        out.send(&parcel)?;
        self.heartbeats = self.heartbeats.saturating_add(1);
        self.since_heartbeat = 0;
        Ok(())
    }

    /// Reply on `message`'s transaction when it was a call; handler errors
    /// become error parcels so the caller always gets an answer.
    fn respond(&self, message: &Message, reply: Result<Parcel>) -> Result<()> {
        let parcel = match reply {
            Ok(parcel) => parcel,
            Err(error) => error_parcel(
                self.interface_id,
                message.method(),
                CODE_HANDLER,
                error.message(),
            )?,
        };
        match message.txn {
            Some(txn) => self.endpoint.reply_or_drop(txn, &parcel),
            None => Ok(()),
        }
    }
}
