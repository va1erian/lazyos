# `sys::webd_net`

The Rhai module for the Messenger interface `os.lazy.webd.net.v1`. Wire-level reference: [os.lazy.webd.net.v1](../../idl/os.lazy.webd.net.v1.md).

The scope that lets a web view load `http:` and `https:` content: a
`CreateView` made through this interface may use the `Web` and `Origins`
scheme policies. Same arguments as `os.lazy.webd.view.v1.CreateView`; the
view is then used through that interface.

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

```rhai
let value = sys::webd_net::new_view_config();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.webd.net.v1 on the wire](../../idl/os.lazy.webd.net.v1.md)
