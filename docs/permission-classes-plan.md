# Plan: access tags and permission classes

**Goal:** let an app ask for "what a read-only viewer needs" or "what a normal
user app needs" without listing every interface, rule by rule. Today a manifest
lists interfaces one at a time (`pkgstore::rules::compile`), each costs one rule
plus one rule per name it is served under, the kernel keeps at most 256 rules
per label, and nothing records which interfaces are credentials, which methods
only read, or which calls are risky. The consent screen's grouping is guessed.

Three pieces, in dependency order:

1. **Access tags in the IDL.** `.midl` declares, next to the code it describes,
   how risky an interface is, which methods only read, and which interfaces are
   never part of a class.
2. **Permission classes.** A manifest names `classes = ["messenger.read"]`; the
   compiler emits one *class rule* and the kernel decides each call against the
   generated tag table. One rule instead of dozens.
3. **Method-level granularity.** Because the tags are per method, the `read`
   class is "every `@readonly` method", not "whole interfaces".

Status: **shelved, nothing implemented** (decided 2026-10-21). The design below
is complete enough to resume from; no code was written. See "Why shelved" and
"How to resume" at the end. Motivation:
[`messenger-playground-plan.md`](messenger-playground-plan.md), which went with
the Terminal's authority instead (its D1 and D7).

---

## 1. Syntax

Attributes sit after the doc comment and before the declaration, one per line:

```idl
/// The hierarchical configuration registry.
@risk(medium)
interface os.lazy.confd.v1 {
    /// Read one value.
    @readonly
    method Get(path: String = 1) -> (value: Option<Value> = 1);

    /// Remove a value.
    @risk(high)
    method Delete(path: String = 1) -> ();
}
```

| Attribute | On | Meaning |
|---|---|---|
| `@risk(low\|medium\|high)` | interface (**required**), method (optional override) | How much damage a call can do. *low*: reads, or tiny reversible effects. *medium*: changes the user's own persistent state or reaches outside the app (notifications, clipboard writes, opening files, volume). *high*: affects other users or the system (stopping services, installing, network and clock configuration, mounts, device claims). A method without its own `@risk` has its interface's. |
| `@protected` | interface | Never part of any class. A manifest can still list it in `interfaces` and the consent screen shows it as before. Credentials and elevation (`keyd`, `keyd.appsecret`, `accounts`, `elevd`), the compositor and input (`display*`, `input*`), the kernel ACL scopes and `policy.v1`. |
| `@readonly` | method | The call does not change state anywhere (no persistent write, no side effect another party can observe beyond a log line). Never on a `oneway` method. A method that returns a secret is not read-only in this sense: tag the interface `@protected`. |
| `@service("name")` | interface, repeatable | A name the interface is served under besides its own name and its name without `.vN` (replaces `pkgstore::rules::SERVICE_NAMES`). |
| `@implies("scope.name.vN")` | method, repeatable | The call also needs this kernel scope (a capability name the kernel authorizes like an interface, `fnv1a64` of the name, not in `idl/`). Replaces `pkgstore::rules::IMPLIED_SCOPES`. |

An unknown attribute, a missing interface `@risk`, a repeated singleton
attribute, an attribute on a struct, enum, topic or ring, or `@readonly` on a
`oneway` method is a `midlc` error with a line number.

## 2. Classes

Three classes, a strict chain **read ⊂ standard ⊂ full**. A *covered call* is
a `(interface, method)` of a **non-protected** interface:

| Class (manifest spelling) | Covers |
|---|---|
| `messenger.read` | every `@readonly` method |
| `messenger.standard` | `read`, plus every method whose effective risk is `low` or `medium` |
| `messenger.full` | every method of every non-protected interface |

A class also covers, for the interfaces it covers at least one method of:

* **Resolving the names** the interface is served under (its name, its name
  without `.vN`, its `@service` names). The kernel checks `Resolve` by
  `fnv1a32(name)`.
* **Subscribing** to the segments of its declared topics (every literal
  segment, `+` and `#`; the kernel authorizes a topic one segment at a time).
  No class ever grants *publishing*: the app's own `app/<id>/` namespace needs
  no rule, anything else is listed explicitly as `publish:` today.
* The scopes a covered method `@implies`.

A class is a *floor for convenience, not a ceiling*: a manifest may add exact
`interfaces`, `publish:` and `subscribe:` entries beside it.

## 3. The generated table

`midlc` emits `pub mod policy_tags` into `libs/generated/src/lib.rs`
(`messenger_generated::policy_tags`), `no_std`, used by the kernel, `pkgstore`
and `lazypkg`'s tests. The contract:

```rust
pub enum Class { Read, Standard, Full }        // repr order: Read < Standard < Full
impl Class {
    pub const ALL: [Class; 3];
    pub fn name(self) -> &'static str;          // "messenger.read", ...
    pub fn parse(name: &str) -> Option<Class>;
    pub fn rule_id(self) -> u64;                // CLASS_BASE + index
    pub fn from_rule_id(id: u64) -> Option<Class>;
    pub fn implies(self, other: Class) -> bool; // self >= other
}
pub const CLASS_BASE: u64 = 0xFFFF_FFFF_FFFF_FF00;  // ids ..00 ..01 ..02; ..FF is the ANY_INTERFACE wildcard
pub fn is_class_id(id: u64) -> bool;            // in CLASS_BASE..CLASS_BASE+16 (reserved range)

pub enum Risk { Low, Medium, High }
pub struct MethodTag    { pub id: u32, pub name: &'static str, pub readonly: bool, pub risk: Risk }
pub struct InterfaceTag { pub id: u64, pub name: &'static str, pub risk: Risk, pub protected: bool,
                          pub services: &'static [&'static str],   // @service names, not the derived two
                          pub methods: &'static [MethodTag] }
pub static INTERFACES: &[InterfaceTag];         // sorted by id
pub fn interface(id: u64) -> Option<&'static InterfaceTag>;
pub fn service_names(tag: &InterfaceTag) -> impl Iterator<Item = alloc::string::String>; // name, name minus .vN, services

/// The kernel's question. `interface_id` is a real interface, or one of the
/// three scope ids: names.resolve.v1 (then `method` is fnv1a32(name)),
/// topics.subscribe.v1 (then `method` is fnv1a32(segment)), topics.publish.v1
/// (never covered), or an @implies scope.
/// `method == u32::MAX` (ANY_METHOD) is covered only when the class covers
/// every method of the interface (or, for a scope, every hash it can be asked).
pub fn class_covers(class: Class, interface_id: u64, method: u32) -> bool;

/// Whether a rule for `held` covers a rule for `wanted` (a child's class rule
/// under its app's): `held.implies(wanted)`.
```

## 4. Kernel

A `LabelRule` whose `interface_id` is a class id and whose `method` is
`ANY_METHOD` is a *class rule* (`allow` true or false; a deny class rule
shadows later rules like any deny). `Rule::matches` asks `class_covers`.
`load_label` refuses (`LoadError`, `EINVAL` at the op) a class id outside
`Class::ALL`, or a class rule with a method other than `ANY_METHOD`. `covered_by`
(child labels) accepts a child's class rule when the parent holds a rule for the
same or a wider class, and a child's ordinary rule when the parent's class rule
covers it. `label_allowed_ids` (confine) ignores class ids. Class rules count as
one rule each toward the 256 per label. The audit and `LABEL:DENY` lines are
unchanged.

## 5. Manifest and consent

`[permissions] classes = ["messenger.read"]` (`lazypkg::Permissions::classes`,
validated against `Class` names; unknown or duplicate names are manifest
problems). `pkgstore::rules::compile` emits one class rule per class, then the
explicit `interfaces`/topics/network entries as today. `explain` shows each class
as one consent entry with its risk and an expandable list of the interfaces and
methods it covers, from the table. A manifest's rule count falls from
"interfaces plus names" to "classes plus extras".

## 6. Phases

| Phase | Delivers | Owner area |
|---|---|---|
| A1 | Parser, model, validation, `manifest.json` / docs / `policy_tags` emission, midlc tests, `docs/midl.md` | `tools/midlc/`, `docs/midl.md` |
| A2 | Tags on every `idl/*.midl` interface and method, with a rationale table | `idl/` |
| A3 | Regenerate outputs (`libs/generated`, `idl/manifest.json`, `docs/idl`, `libs/rhai-lazy` table and `api/`) | integration |
| B | Kernel: class rules in `acl.rs`, tests (correctness and stress), docs | `kernel/`, `docs/architecture/ipc-security.md` |
| C | `lazypkg` field, `pkgstore` compile and explain, packaged apps may use it, docs | `libs/lazypkg`, `libs/pkgstore`, `docs/packages.md` |
| D | The Playground manifest uses `messenger.standard` (plus extras) | `messenger-playground-plan.md` (dropped: the Playground is Terminal-class) |

---

## 7. Why shelved

The change is real work across five areas (the IDL compiler, every `.midl`
file, the generated crate, the kernel's ACL, and `lazypkg`/`pkgstore`/the
consent screen) and it pays off for apps that do not exist yet. The one app
that motivated it, the Messenger Playground, is a developer tool whose point is
the user's full authority; it is better served by saying so plainly (D7 of its
plan) than by a wide, curated manifest. Nothing else in the tree is blocked by
the 256-rule limit today. So the design is kept, the code is not started.

Nothing was stashed or committed for this: the two implementation agents that
were launched (midlc, and the tagging of `idl/*.midl`) were stopped before they
had written anything into the tree.

## 8. What the survey found (so nobody repeats it)

* **The kernel already has the right primitives.** `kernel/src/ipc/acl.rs`
  rules are ordered, first match wins, with `ANY_INTERFACE`/`ANY_METHOD`
  wildcards and explicit denies. Per-label storage is 256 rules
  (`MAX_RULES_PER_LABEL`), 4096 in total. Method-level allow is already
  expressible (one rule per method); what is missing is any *grouping*.
* **Rule cost is dominated by name resolution, not interfaces.** An interface
  entry compiles to one `ANY_METHOD` rule plus one `names.resolve` rule per name
  it is served under (`rules.rs::service_names`: the interface name, the name
  without `.vN`, and extras such as `os.lazy.accountsd`): 3 to 4 rules per
  interface. About 40 service interfaces is already ~150 rules before any topic
  segment. A class rule has to cover resolution and topic segments too, which is
  why `class_covers` takes the scope ids (section 3).
* **Topics are authorized per segment**
  (`topics.subscribe.v1`/`publish.v1` scopes with `fnv1a32(segment)`), so a
  grant for `system/#` is the segments `system` and `#`, and a filter with a
  free tail (`system/confd/changed/sys/ui/#`) needs `sys` and `ui` as well.
  Classes grant the union of the *declared* topics' segments only. Subtree
  grants (below) would remove this limit.
* **A labelled app cannot serve `os.lazy.echo.v1`**, and `msg::serve` does not
  send the `interface_names` the kernel requires of a labelled task
  (`docs/messenger.md` section 8, "Interface domains"; the LazyRAD sample wraps
  it in `try`/`catch`). Relevant to any scripting tool that is a package.
* **Child labels** (`[processes.*]`) already enforce "at most the app's rules"
  (`acl::covered_by`, `evaluate_label` intersecting parent and child); a class
  rule needs the `implies` case there (section 4).
* **IDs and hashes.** Interface ids are `fnv1a64(name)` and name/segment checks
  use `fnv1a32`; a class id range at the top of the `u64` space (below the
  `ANY_INTERFACE` wildcard) is safe if the generator asserts no real id falls in
  it.
* **Existing hand-kept tables the tags would replace:**
  `pkgstore::rules::SERVICE_NAMES` (4 entries) and `IMPLIED_SCOPES` (clipboard
  read/write).
* **Consent screen:** `pkgstore::explain` already groups by risk, from a
  hand-kept table; tags would make that table generated.

## 9. Ideas considered and not planned

From the design discussion that produced this plan; each is independent of the
three pieces above.

* **Subtree topic grants.** `subscribe:system/**` evaluated as a prefix, with
  denies (`user/*/`, secret topics) taking precedence. Removes the
  segment-union limit above.
* **Grant on denial.** The manifest stays small; a refused call carries a
  "grantable" marker, the app asks `pkgd` to raise a trusted prompt through
  `xuid` ("Allow once / this session / always"), session grants live in `pkgd`
  memory and are revoked at logout like `Develop` approvals. `@protected`
  interfaces are never grantable. No kernel blocking: the app retries after the
  grant.
* **Per-run labels attenuated by the app's ceiling.** Each script run gets a
  transient child label computed by `rhai_lazy::msg::permissions`, inside the
  manifest's ceiling; names built at run time fall back to grant-on-denial.
* **A `devtool` role.** `[entry] role = "devtool"` compiles to "whatever the
  user's uid could do over Messenger except `@protected`": one consent line,
  with kernel attribution, audit and revocation that an unlabelled app lacks.
  This is the natural successor to the Terminal exception and the strictest to
  review. **Now planned, the small way** (default allow plus a reviewed deny
  list, no tags or classes needed): [`devtool-packages-plan.md`](devtool-packages-plan.md).
* **Expiring grants, a Permissions page in Settings, an audit line and a tray
  badge for broad-role apps.**

## 10. How to resume

1. Read sections 1 to 6 as the contract. The decisions most worth re-checking
   first: the three-class chain (is `standard` = low and medium right?), that
   `@protected` interfaces can still be named explicitly in a manifest, and
   `@implies` on methods.
2. Do A1 (midlc) and A2 (tagging every `idl/*.midl`, with a rationale table in
   `docs/permission-classes-tags.md`) in parallel; they touch disjoint files.
   Regenerate (A3) with the command CI runs:
   `python tools/midlc/midlc.py --check --out libs/generated/src/lib.rs --schema libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api --manifest idl/manifest.json --docs docs/idl idl/*.midl`
   (drop `--check` to write).
3. Then B (kernel, with correctness and stress tests in `kernel/src/tests/` per
   AGENTS.md) and C (`lazypkg`, `pkgstore`, `explain`, `docs/packages.md`) in
   parallel; both depend on the generated `policy_tags` module.
4. `idl/conformance/` is the midlc test corpus: its valid files need `@risk`
   added when the tag becomes required.
