//! `nicctl soak=<n>`: attach, exchange one ARP request with the gateway,
//! detach, `n` times in a row.
//!
//! A leak of anything per attachment (a handle, a mapping, a shared buffer, a
//! DMA slot) shows up in two places: the fabric snapshot, and the driver's own
//! counters, where every transmit slot must come back and no ring error may
//! appear. The traffic is real (one ARP exchange per iteration), so it is in
//! the packet capture too.
//!
//! **Whose counts.** Handles and shared buffers are judged per task: those
//! of this task and of the driver, the only two an attachment touches, must
//! be exactly what they were before the first iteration. The system-wide
//! totals are not: the rest of the system keeps working during the soak
//! (`init` restarting a service, launching one late, `pkgd` starting its
//! apps), and whatever handles that takes or gives back would be blamed on
//! the driver. What the others did is printed (`NICCTL:SOAK:OTHERS`) so a
//! run shows it. Mappings and endpoints have no per-task count; the per-task
//! handles cover them: every endpoint an attachment makes is reached through
//! a handle of one of the two tasks, and a shared buffer is mapped only while
//! a buffer handle (a handle like any other) to it is open in that task.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{fabric_stats, FabricStats};
use user::task_snapshot::{task_snapshot, TaskSnapshot};

use super::common::{arp_exchange, connect, fail, GATEWAY_IP};

/// Ring size used by every iteration.
const SLOTS: u32 = 16;

/// Run `iterations` attach/exchange/detach cycles; returns how many completed.
pub(super) fn run(iterations: u32) -> Result<u32, String> {
    let client = connect()?;
    let info = client.info().map_err(fail("info"))?;
    let mac: [u8; 6] = info
        .mac
        .as_slice()
        .try_into()
        .map_err(|_| String::from("the driver reported a bad MAC"))?;
    let involved = Involved::find()?;
    let before = fabric_stats().map_err(fail("fabric stats"))?;
    let tasks_before = task_snapshot().map_err(fail("task snapshot"))?;
    let stats_before = client.stats().map_err(fail("stats"))?;
    for round in 0..iterations {
        let what = |step: &str| format!("iteration {round}: {step}");
        let mut attachment = client.attach(SLOTS).map_err(fail(&what("attach")))?;
        arp_exchange(&client, &mut attachment, mac, GATEWAY_IP).map_err(|e| what(&e))?;
        client
            .detach(attachment.ring)
            .map_err(fail(&what("detach")))?;
        attachment.close();
    }
    let after = fabric_stats().map_err(fail("fabric stats"))?;
    let tasks_after = task_snapshot().map_err(fail("task snapshot"))?;
    let stats_after = client.stats().map_err(fail("stats"))?;
    involved.same_tasks(&tasks_before, &tasks_after)?;
    report_others(&involved, &before, &after, &tasks_after);
    for (who, slot) in involved.named() {
        let (was, now) = (before.tasks[slot], after.tasks[slot]);
        for (what, a, b) in [
            ("handles", was.handles, now.handles),
            ("shared buffers", was.buffers, now.buffers),
            ("buffer bytes", was.buffer_bytes, now.buffer_bytes),
        ] {
            if a != b {
                return Err(format!(
                    "{who}: {what} went from {a} to {b} over {iterations} cycles"
                ));
            }
        }
    }
    if stats_after.ring_errors != stats_before.ring_errors {
        return Err(String::from(
            "the driver counted ring errors during the soak",
        ));
    }
    let sent = stats_after.tx_frames - stats_before.tx_frames;
    if sent < u64::from(iterations) {
        return Err(format!(
            "the driver transmitted {sent} frames for {iterations} requests"
        ));
    }
    Ok(iterations)
}

/// The two task slots an attachment touches.
struct Involved {
    /// This task.
    me: usize,
    /// The driver, `netdrv`.
    driver: usize,
}

impl Involved {
    /// Find both slots from the task list (a task is named after its
    /// program): the only live `nicctl` and the only live `netdrv`. The
    /// registry, which knows who serves [`api::NAME`], is not open to a
    /// confined `nicctl`. One of each is also what the soak needs: a second
    /// `nicctl` would share the driver with it (one attachment at a time),
    /// and with two drivers the counts could belong to the other card.
    fn find() -> Result<Involved, String> {
        let tasks = task_snapshot().map_err(fail("task snapshot"))?;
        Ok(Involved {
            me: only(&tasks, fhs::bin::NICCTL)?,
            driver: only(&tasks, fhs::bin::NETDRV)?,
        })
    }

    /// `(who, slot)` for both.
    fn named(&self) -> [(&'static str, usize); 2] {
        [("nicctl", self.me), ("the driver", self.driver)]
    }

    /// Neither slot may have changed hands during the soak: the counts of a
    /// different task in the same slot would mean nothing.
    fn same_tasks(&self, before: &TaskSnapshot, after: &TaskSnapshot) -> Result<(), String> {
        for (who, slot) in self.named() {
            let identity = |tasks: &TaskSnapshot| {
                tasks
                    .rows
                    .get(slot)
                    .filter(|row| row.live)
                    .map(|row| (row.ppid, row.name.clone()))
            };
            let (old, new) = (identity(before), identity(after));
            if old.is_none() || old != new {
                return Err(format!("{who} (slot {slot}) did not live through the soak"));
            }
        }
        Ok(())
    }
}

/// The slot of the one live task running `program` (the kernel names a task
/// after its program's basename).
fn only(tasks: &TaskSnapshot, program: &str) -> Result<usize, String> {
    let name = program.rsplit('/').next().unwrap_or(program);
    let slots: Vec<usize> = tasks
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.live && row.name == name)
        .map(|(slot, _)| slot)
        .collect();
    match slots.as_slice() {
        [slot] => Ok(*slot),
        _ => Err(format!(
            "{} live {name} tasks; the soak needs exactly one",
            slots.len()
        )),
    }
}

/// Print what the rest of the system did to the fabric meanwhile: the
/// system-wide totals and every other task whose handle count moved.
fn report_others(
    involved: &Involved,
    before: &FabricStats,
    after: &FabricStats,
    tasks: &TaskSnapshot,
) {
    let mut line = format!(
        "NICCTL:SOAK:OTHERS handles {}->{} endpoints {}->{} buffers {}->{} mappings {}->{}",
        before.handles,
        after.handles,
        before.endpoints,
        after.endpoints,
        before.buffers,
        after.buffers,
        before.buffer_mappings,
        after.buffer_mappings
    );
    let slots = before.handles_per_task.iter().zip(&after.handles_per_task);
    for (slot, (&old, &new)) in slots.enumerate() {
        if old == new || slot == involved.me || slot == involved.driver {
            continue;
        }
        // A task that exited meanwhile has left an empty slot.
        let name = tasks
            .rows
            .get(slot)
            .filter(|row| row.live)
            .map_or("(exited)", |row| row.name.as_str());
        line.push_str(&format!(" {name}@{slot}:{old}->{new}"));
    }
    line.push('\n');
    user::sys::write_str(&line);
}
