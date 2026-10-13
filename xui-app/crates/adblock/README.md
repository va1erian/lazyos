# lazyadblock

Adblock Plus filter lists to WebKit content-rule lists, for LazyWeb+'s ad blocker
(issue #845). Portable and host-tested; `convert(text, &Options::default())` returns the
rule lists (`WKUserContentExtensionStoreCompile`'s `jsonSource`) and a `Stats`.

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazyadblock
FUZZ_CASES=100000 cargo test --manifest-path xui-app/Cargo.toml -p lazyadblock --release
FUZZ_SEED=0x<seed> cargo test ... -p lazyadblock <test>    # replay a printed failure
```

## What is converted

| Filter | Rule |
|---|---|
| `\|\|host^`, `\|url`, `url\|`, `*`, `^`, literals | `block` with a `url-filter` in WebKit's regex subset |
| `$script,image,stylesheet,font,media,xmlhttprequest,subdocument,document,ping,websocket,popup,other,all` and `~` forms | `resource-type` (no type option: everything but top-level documents and popups, as Adblock Plus) |
| `$third-party`, `$~third-party`, `$first-party` | `load-type` |
| `$domain=a.com\|b.org` or `$domain=~a.com` | `if-domain` / `unless-domain` with `*` (subdomains too); a mix is dropped |
| `$match-case` | `url-filter-is-case-sensitive` |
| `$important` | a block placed after the exceptions |
| `@@...` | `ignore-previous-rules`, after the blocks; repeated in every part of a split list |
| `@@...$document` | `ignore-previous-rules` on every request while the top-level URL matches |
| `$badfilter` | removes the filter it names |
| `/regex/` | the same regex when it is inside the subset (below); `/word/` is a path |

## What is dropped (counted in `Stats`, never approximated)

* element hiding (`##`, `#@#`, `#?#`, `$elemhide` and the other `hide` options): `cosmetic`;
* `$csp`, `$redirect`, `$removeparam`, `$to`, `$denyallow`, `$header`, `$replace`, wildcard
  domains, ...: `unsupported-option`; scriptlets (`##+js`) are cosmetic lines and are never run;
* regex with `|`, `{n,m}`, `\b`, `\s`, lazy quantifiers, lookarounds, reversed class ranges:
  `unsupported-pattern`;
* non-ASCII patterns: `non-ascii` (the list would need punycode);
* `domain=` with both included and excluded domains: `mixed-domains`;
* `$object`/`$object-subrequest` (WebKit has no plugin type; `other` would be wider), a pattern that is only anchors
  and wildcards with no type, party or domain, and patterns over 2048 characters or with over 16 wildcards:
  `unsupported-*`. A dropped exception (`dropped_exceptions`) is the one drop that blocks *more*, so it is
  counted apart.

## The WebKit subset, and why it is checked twice

One pattern WebKit's `URLFilterParser` refuses fails the *whole* list (`JSONInvalidRegex`).
It accepts ASCII literals, `.`, classes, `*` `+` `?`, groups **without** alternation, `^`
first and `$` last. `tests/common/mod.rs` has an independent parser and matcher for that
subset and an Adblock Plus matcher written without the converter's code; the differential
tests compare the two on random and near-miss URLs, and the fuzz tests parse every emitted
rule. A trailing `^` matches a separator character, not the end of the URL, so such a filter
can only block less than Adblock Plus would.
