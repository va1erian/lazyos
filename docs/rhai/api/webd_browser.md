# `sys::webd_browser`

The Rhai module for the Messenger interface `os.lazy.webd.browser.v1`. Wire-level reference: [os.lazy.webd.browser.v1](../../idl/os.lazy.webd.browser.v1.md).

The scope of a web browser: everything `os.lazy.webd.net.v1` allows, plus
the shared `browser` profile kept on disk, downloads into the person's
Downloads folder, content blocking and `Handoff.Browser`.

Not callable from a script (the request carries a kernel object): `CreateView`.

## Types

### `ViewConfig`

`os.lazy.webd.view.v1.ViewConfig`, field for field.

| Name | IDL type | Script value |
|---|---|---|
| `profile` | `String` | string |
| `ephemeral` | `Bool` | bool |
| `scheme_policy` | `U32` | int |
| `origins` | `Array<String>` | array of string |
| `scripts` | `Bool` | bool |
| `handoff` | `U32` | int |
| `downloads` | `Bool` | bool |
| `popups` | `Bool` | bool |
| `blocking` | `Bool` | bool |
| `notifications` | `Bool` | bool |
| `user_agent_suffix` | `String` | string |
| `deltas` | `Bool` | bool |
| `dialogs` | `Bool` | bool |
| `notify_in_client` | `Bool` | bool |

```rhai
let value = sys::webd_browser::new_view_config();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.webd.browser.v1 on the wire](../../idl/os.lazy.webd.browser.v1.md)
