// title: Who is on the fabric?
// tags: explore, registry
// needs: none
// expect: os.lazy.confd  uid=0  label=0  ifaces=1
// expect: os.lazy.timed  uid=0  label=0  ifaces=1
// expect: zeta.app  uid=1000  label=7  ifaces=0
//
// A sorted tour of the registry with its owners. The registry reply has one
// field, so list() returns the array itself.
let entries = sys::messenger_registry::list();
entries.sort(|a, b| if a.name < b.name { -1 } else if a.name > b.name { 1 } else { 0 });
for e in entries {
    print(`${e.name}  uid=${e.owner_uid}  label=${e.owner_label}  ifaces=${e.interfaces.len()}`);
}
