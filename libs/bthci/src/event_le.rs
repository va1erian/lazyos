//! The LE Meta event: connection complete (plain and enhanced), advertising
//! reports and long term key requests.

use alloc::vec::Vec;

use crate::addr::BdAddr;
use crate::event::{AdvReport, Event, MAX_ADV_DATA, MAX_ADV_REPORTS};
use crate::opcode as op;
use crate::wire::{put_u16, put_u64, Reader};
use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeConnection {
    pub status: u8,
    pub handle: u16,
    /// 0: we are the central.
    pub role: u8,
    pub peer_address_type: u8,
    pub peer: BdAddr,
    pub interval: u16,
    pub latency: u16,
    pub supervision_timeout: u16,
    pub clock_accuracy: u8,
}

impl LeConnection {
    pub(crate) fn parse(r: &mut Reader<'_>) -> Result<LeConnection, Error> {
        Ok(LeConnection {
            status: r.u8()?,
            handle: r.u16()?,
            role: r.u8()?,
            peer_address_type: r.u8()?,
            peer: r.addr()?,
            interval: 0,
            latency: 0,
            supervision_timeout: 0,
            clock_accuracy: 0,
        })
    }

    /// The timing tail, which follows the RPAs in the enhanced layout.
    pub(crate) fn parse_timing(&mut self, r: &mut Reader<'_>) -> Result<(), Error> {
        self.interval = r.u16()?;
        self.latency = r.u16()?;
        self.supervision_timeout = r.u16()?;
        self.clock_accuracy = r.u8()?;
        Ok(())
    }

    pub(crate) fn put_head(&self, p: &mut Vec<u8>) {
        p.push(self.status);
        put_u16(p, self.handle);
        p.extend_from_slice(&[self.role, self.peer_address_type]);
        p.extend_from_slice(&self.peer.0);
    }

    pub(crate) fn put_timing(&self, p: &mut Vec<u8>) {
        put_u16(p, self.interval);
        put_u16(p, self.latency);
        put_u16(p, self.supervision_timeout);
        p.push(self.clock_accuracy);
    }
}

/// Encode the LE Meta variants (the subevent octet and its parameters).
pub(crate) fn put(ev: &Event, p: &mut Vec<u8>) {
    use Event::*;
    match ev {
        LeConnectionComplete(c) => {
            p.push(op::LE_CONNECTION_COMPLETE);
            c.put_head(p);
            c.put_timing(p);
        }
        LeEnhancedConnectionComplete {
            conn,
            local_rpa,
            peer_rpa,
        } => {
            p.push(op::LE_ENHANCED_CONNECTION_COMPLETE);
            conn.put_head(p);
            p.extend_from_slice(&local_rpa.0);
            p.extend_from_slice(&peer_rpa.0);
            conn.put_timing(p);
        }
        LeAdvertisingReport(reports) => {
            p.push(op::LE_ADVERTISING_REPORT);
            let n = reports.len().min(MAX_ADV_REPORTS);
            p.push(n as u8);
            for r in &reports[..n] {
                p.extend_from_slice(&[r.event_type, r.address_type]);
                p.extend_from_slice(&r.addr.0);
                let d = r.data.len().min(MAX_ADV_DATA);
                p.push(d as u8);
                p.extend_from_slice(&r.data[..d]);
                p.push(r.rssi as u8);
            }
        }
        LeLongTermKeyRequest {
            handle,
            random,
            ediv,
        } => {
            p.push(op::LE_LONG_TERM_KEY_REQUEST);
            put_u16(p, *handle);
            put_u64(p, *random);
            put_u16(p, *ediv);
        }
        UnknownLe { subevent, params } => {
            p.push(*subevent);
            p.extend_from_slice(params);
        }
        _ => {}
    }
}

pub(crate) fn parse(r: &mut Reader<'_>) -> Result<Event, Error> {
    let sub = r.u8()?;
    Ok(match sub {
        op::LE_CONNECTION_COMPLETE => {
            let mut c = LeConnection::parse(r)?;
            c.parse_timing(r)?;
            Event::LeConnectionComplete(c)
        }
        op::LE_ENHANCED_CONNECTION_COMPLETE => {
            let mut conn = LeConnection::parse(r)?;
            let local_rpa = r.addr()?;
            let peer_rpa = r.addr()?;
            conn.parse_timing(r)?;
            Event::LeEnhancedConnectionComplete {
                conn,
                local_rpa,
                peer_rpa,
            }
        }
        op::LE_ADVERTISING_REPORT => {
            let n = usize::from(r.u8()?);
            if n > MAX_ADV_REPORTS {
                return Err(Error::TooLong);
            }
            let mut reports = Vec::with_capacity(n);
            for _ in 0..n {
                let event_type = r.u8()?;
                let address_type = r.u8()?;
                let addr = r.addr()?;
                let dlen = usize::from(r.u8()?);
                if dlen > MAX_ADV_DATA {
                    return Err(Error::TooLong);
                }
                let data = r.take(dlen)?.to_vec();
                reports.push(AdvReport {
                    event_type,
                    address_type,
                    addr,
                    data,
                    rssi: r.u8()? as i8,
                });
            }
            Event::LeAdvertisingReport(reports)
        }
        op::LE_LONG_TERM_KEY_REQUEST => Event::LeLongTermKeyRequest {
            handle: r.u16()?,
            random: r.u64()?,
            ediv: r.u16()?,
        },
        _ => Event::UnknownLe {
            subevent: sub,
            params: r.rest().to_vec(),
        },
    })
}
