//! [`Mock`]: a scripted [`Transport`] for host tests (feature `mock`, or this
//! crate's own tests). It records every resolve, call and send, answers
//! calls from a script, and can kill and restart a service so a test sees
//! the reconnect.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use super::connection::{Handle, Transport};
use super::deadline::Deadline;
use super::errno::Errno;
use crate::{Header, Object, Parcel};

/// A call or send the mock saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// The handle it went out on.
    pub handle: Handle,
    /// The request header.
    pub header: Header,
    /// The request body.
    pub body: Vec<u8>,
    /// The objects the request moved or shared.
    pub objects: Vec<Object>,
    /// The deadline the caller gave (`None` for a one-way send).
    pub until: Option<Deadline>,
}

struct Service {
    name: String,
    generation: u64,
    alive: bool,
}

#[derive(Default)]
struct State {
    services: Vec<Service>,
    /// `(handle, name, generation)` of every handle handed out and not
    /// released.
    handles: Vec<(Handle, String, u64)>,
    next_handle: Handle,
    next_generation: u64,
    script: VecDeque<Result<Parcel, Errno>>,
    resolves: Vec<(String, Deadline)>,
    /// Names that appear while a resolve waits for them.
    appearing: Vec<String>,
    calls: Vec<Recorded>,
    released: Vec<Handle>,
}

/// A transport that talks to nobody. See the module text.
pub struct Mock {
    state: RefCell<State>,
    bound: Deadline,
}

impl Default for Mock {
    fn default() -> Self {
        Mock::new()
    }
}

impl Mock {
    /// A mock with no service registered.
    pub fn new() -> Mock {
        Mock {
            state: RefCell::new(State {
                next_handle: 100,
                ..State::default()
            }),
            bound: Deadline::secs(10),
        }
    }

    /// The connect bound [`Transport::connect_bound`] reports.
    pub fn with_connect_bound(mut self, bound: Deadline) -> Mock {
        self.bound = bound;
        self
    }

    /// Register `name` with a live peer (replacing a dead one).
    pub fn register(&self, name: &str) {
        let mut state = self.state.borrow_mut();
        state.next_generation += 1;
        let generation = state.next_generation;
        state.services.retain(|service| service.name != name);
        state.services.push(Service {
            name: name.to_string(),
            generation,
            alive: true,
        });
    }

    /// The peer of `name` dies but its registration stays, as when a service
    /// crashes before the registry reaps it: every handle already resolved
    /// fails with `EPIPE`, and so does a new one until [`Mock::register`].
    pub fn kill(&self, name: &str) {
        for service in &mut self.state.borrow_mut().services {
            if service.name == name {
                service.alive = false;
            }
        }
    }

    /// `name` registers while a resolve waits for it (a resolve with any
    /// deadline but [`Deadline::Poll`], as the kernel's `WaitFor` would
    /// return when the service starts), once.
    pub fn registers_when_waited_for(&self, name: &str) {
        self.state.borrow_mut().appearing.push(name.to_string());
    }

    /// The registration of `name` is reaped: a resolve fails with `ENOENT`.
    pub fn unregister(&self, name: &str) {
        self.state
            .borrow_mut()
            .services
            .retain(|service| service.name != name);
    }

    /// Queue the reply of the next call.
    pub fn reply(&self, reply: Parcel) {
        self.state.borrow_mut().script.push_back(Ok(reply));
    }

    /// Queue a transport failure for the next call.
    pub fn fail(&self, errno: Errno) {
        self.state.borrow_mut().script.push_back(Err(errno));
    }

    /// Every call and send so far, oldest first.
    pub fn calls(&self) -> Vec<Recorded> {
        self.state.borrow().calls.clone()
    }

    /// Every resolve so far, with the bound it was given.
    pub fn resolves(&self) -> Vec<(String, Deadline)> {
        self.state.borrow().resolves.clone()
    }

    /// Handles given back with [`Transport::release`].
    pub fn released(&self) -> Vec<Handle> {
        self.state.borrow().released.clone()
    }

    /// Handles still held.
    pub fn open_handles(&self) -> usize {
        self.state.borrow().handles.len()
    }

    fn peer_alive(state: &State, handle: Handle) -> Result<(), Errno> {
        let (_, name, generation) = state
            .handles
            .iter()
            .find(|(open, ..)| *open == handle)
            .ok_or(Errno::BADF)?;
        let service = state.services.iter().find(|s| &s.name == name);
        match service {
            Some(service) if service.alive && service.generation == *generation => Ok(()),
            _ => Err(Errno::PIPE),
        }
    }
}

impl Transport for Mock {
    fn connect_bound(&self) -> Deadline {
        self.bound
    }

    fn call_flags(&self) -> u16 {
        0
    }

    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno> {
        let waits = until != Deadline::Poll;
        let appears = {
            let mut state = self.state.borrow_mut();
            state.resolves.push((name.to_string(), until));
            let known = state.services.iter().any(|service| service.name == name);
            let late = state.appearing.iter().position(|late| late == name);
            match late {
                Some(index) if waits && !known => {
                    state.appearing.remove(index);
                    true
                }
                _ => false,
            }
        };
        if appears {
            self.register(name);
        }
        let mut state = self.state.borrow_mut();
        // The kernel's wait ends with the timeout where a plain resolve is
        // `ENOENT`.
        let missing = if waits { Errno::TIMEDOUT } else { Errno::NOENT };
        let generation = state
            .services
            .iter()
            .find(|service| service.name == name)
            .map(|service| service.generation)
            .ok_or(missing)?;
        state.next_handle += 1;
        let handle = state.next_handle;
        state.handles.push((handle, name.to_string(), generation));
        Ok(handle)
    }

    fn call(&self, handle: Handle, request: &Parcel, until: Deadline) -> Result<Parcel, Errno> {
        let mut state = self.state.borrow_mut();
        state.calls.push(Recorded {
            handle,
            header: request.header,
            body: request.body.clone(),
            objects: request.objects.clone(),
            until: Some(until),
        });
        Mock::peer_alive(&state, handle)?;
        state
            .script
            .pop_front()
            .unwrap_or_else(|| Ok(Parcel::default()))
    }

    fn send(&self, handle: Handle, request: &Parcel) -> Result<(), Errno> {
        let mut state = self.state.borrow_mut();
        state.calls.push(Recorded {
            handle,
            header: request.header,
            body: request.body.clone(),
            objects: request.objects.clone(),
            until: None,
        });
        Mock::peer_alive(&state, handle)
    }

    fn release(&self, handle: Handle) {
        let mut state = self.state.borrow_mut();
        state.handles.retain(|(open, ..)| *open != handle);
        state.released.push(handle);
    }
}
