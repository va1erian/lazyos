//! RAII ownership of a freshly built user page table (issue #229).
//!
//! Split out of `mem/mod.rs` (issue #194). Building a fresh address space
//! (`new_user_table`) and then loading an image into it can fail half-way,
//! after frames are mapped. A caller that keeps the table in a raw `PhysAddr`
//! must remember to call [`free_user_table`](super::free_user_table) on every
//! error path; forgetting one leaks the whole address space, which is exactly
//! what `execve` did. Holding the table in this guard makes the cleanup
//! automatic: dropping it frees the table and its frames, and
//! [`commit`](UserTableGuard::commit) disarms it once the table is installed in
//! the task and must outlive the call.

use x86_64::PhysAddr;

#[must_use = "the guard frees the table on drop; commit it once the switch is done"]
pub struct UserTableGuard(Option<PhysAddr>);

impl UserTableGuard {
    /// Take ownership of a freshly built `table`.
    pub fn new(table: PhysAddr) -> Self {
        Self(Some(table))
    }

    /// The guarded table, for installing it (e.g. `switch_to`, `set_pml4`).
    pub fn table(&self) -> PhysAddr {
        // INVARIANT: the inner option is only cleared in `commit`/`drop`, both
        // of which consume or end the guard, so a live guard always holds one.
        self.0.expect("UserTableGuard used after commit")
    }

    /// Commit the table to the task: drop the guard without freeing it. The
    /// caller now owns `table` and must free it when the address space dies.
    pub fn commit(mut self) {
        self.0 = None;
    }
}

impl Drop for UserTableGuard {
    fn drop(&mut self) {
        if let Some(table) = self.0.take() {
            super::free_user_table(table);
        }
    }
}
