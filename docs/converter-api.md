# Converter API v1

MathPreview separates source conversion from the browser viewer:

```text
document or editor buffer -> converter -> viewer-ready document -> browser viewer
```

The converter owns source-language behavior. It returns HTML blocks, source
locations, dependencies, assets, diagnostics, and the runtime settings needed
to place those blocks in a viewer. The live host owns shell construction,
scheduling, watches, block diff decisions, WebSocket transport, source
navigation endpoints, and asset processing. Browser code applies updates,
runs controls and source interactions, and owns MathJax typesetting.

MathPreview bundles LaTeX and Markdown converters. Applications written in
Rust can call the in-process contract, while other languages can use the
persistent JSON protocol exposed by `mathpreview-cli convert`.

The converter API version is independent of MathPreview's WebSocket protocol.
Adding a converter implementation does not by itself change browser messages.
Changing head-level runtime requirements or switching the active converter may
still require a full shell reload.

See [Converter and viewer architecture](architecture.md) for the complete
ownership map, live update flow, artifact invariants, current limitations, and
extension checklist. In particular, v1 is a viewer artifact boundary, not yet
a loadable converter plugin system.

## Bundled converters

| ID | Extensions | Root buffer | Additional buffers | Source sync | Assets |
| --- | --- | --- | --- | --- | --- |
| `latex` | `.tex`, `.ltx` | yes | includes, preamble files, and bibliographies | yes, including math rows | TikZ source payloads |
| `markdown` | `.md`, `.markdown` | yes | no | yes | none |

Automatic selection keeps the existing compatibility rule. `.md` and
`.markdown` select Markdown, while every other extension falls back to the
LaTeX project converter. A protocol request can explicitly select `latex` or
`markdown` when its logical path uses another extension.

## Rust API

The in-process contract lives in `mathpreview_core::converter`:

```rust
use mathpreview_core::{
    converter_for_path, ConversionRequest, HtmlOptions,
};

let request = ConversionRequest::from_source(
    "notes.md",
    "# Notes\n\nInline math: $x^2$.\n",
);
let converter = converter_for_path(&request.path);
let output = converter
    .convert(request, &HtmlOptions::default())
    .expect("conversion succeeds");

assert_eq!(converter.metadata().api_version, 1);
assert!(!output.blocks.is_empty());
```

`DocumentConverter` is object-safe, so a custom Rust host can keep converters
behind `Box<dyn DocumentConverter>`. Each converter declares a stable ID,
extensions, format, API version, and explicit capabilities. A viewer should
check those capabilities and degrade cleanly instead of guessing support from
one non-empty result. MathPreview's bundled selector and live server currently
know only LaTeX and Markdown. They do not provide runtime registration for a
custom implementation.

Most record-shaped converter artifact structs declared in `converter.rs` are
non-exhaustive. Open string wrappers and some aliased renderer and sync wire
types remain exhaustive Rust structs even though they expose construction or
recording APIs. Custom Rust converters should prefer the provided constructors
and `with_*` builders. An additive JSON change still needs a Rust semantic
versioning review. JSON clients should ignore fields they do not recognize.

The older `render_document` and `render_document_from_source` functions keep
their full-page `RenderOutput` contract. They remain on the established render
path so existing TeX and Markdown callers get byte-compatible shell behavior.
Regression tests compare their body, sync map, block patch state, and assets
with the bundled converters. The wire API below omits the shell and parser AST.

## Persistent NDJSON protocol

Start one process and keep its stdin and stdout open:

```sh
mathpreview-cli convert
```

Write one JSON object per line. The process writes one response for every
nonblank input line and flushes it immediately. Newlines inside `source` must
therefore be JSON escapes (`\n`), not literal record separators.

Requests are processed synchronously and sequentially. Keeping the process
alive saves startup cost, but it does not create a document session, parser
cache, cancellation channel, or out-of-order response stream. Hosts should
debounce edits and use `id` to discard stale results. A record error keeps the
stream alive. An I/O failure, panic, signal, or process exit does not produce a
framed response, so the host must restart the process. Stdout is reserved for
NDJSON and logs belong on stderr.

Input records and layer counts are bounded. Response size, block count, asset
payload size, conversion time, and memory are not bounded by v1. An embedding
host should apply its own deadline and output limits when invoking an
untrusted or external implementation.

A one-shot shell example is:

```sh
printf '%s\n' \
  '{"protocol":"mathpreview.converter/v1","id":"edit-1","path":"notes.md","source":"# Notes\n\nInline math: $x^2$."}' \
  | mathpreview-cli convert
```

Keeping the process alive avoids a new binary startup on every editor change.
Each record is limited to 64 MiB. An invalid, unsupported, or oversized record
gets its own error response. It does not terminate or desynchronize the
remaining stream.

### Request

The v1 request fields are:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `protocol` | string | yes | Must be `mathpreview.converter/v1`. |
| `id` | string | yes | Value chosen by the caller and echoed in the response. |
| `path` | path string | yes | Document entry point, or the logical source path for an unsaved buffer. |
| `source` | string or `null` | no | Current root-buffer text. When omitted or null, the converter reads `path` from disk. |
| `file_overrides` | object from path strings to strings | no | Unsaved included, preamble, or bibliography text. LaTeX accepts these. Markdown accepts only its root through `source` or a matching root entry. |
| `converter` | string or `null` | no | `auto` (default), `latex`, or `markdown`. |
| `options` | object | no | Per-request settings described below. |

For a live multi-file TeX project, send the root buffer in `source` and any
other unsaved files in `file_overrides`:

```json
{
  "protocol": "mathpreview.converter/v1",
  "id": "edit-42",
  "path": "/work/paper/main.tex",
  "source": "\\documentclass{article}\n\\input{defs}\n\\begin{document}$\\RR$\\bibliography{refs}\\end{document}\n",
  "file_overrides": {
    "/work/paper/defs.tex": "\\newcommand{\\RR}{\\mathbb{R}}\n",
    "/work/paper/refs.bib": "@book{key, title={Unsaved title}}\n"
  }
}
```

If `source` and `file_overrides` both name the root, `source` wins. An override
does not need to exist on disk. The LaTeX converter normalizes existing path
aliases and also handles missing leaves below an existing project directory.

`options` accepts:

| Field | Type | Meaning |
| --- | --- | --- |
| `title` | string or `null` | Viewer title fallback. The document's own short title still wins when present. |
| `config_files` | path string array | Extra TOML config paths, applied in array order after process-level files. |
| `macro_files` | path string array | Extra macro override paths, applied in array order after process-level files. |
| `mathjax_url` | string or `null` | MathJax script URL for this result. It overrides process-level `--mathjax-url`. |
| `local_asset_base` | string or `null` | Prefix for relative Markdown image paths, such as `/assets/`. |
| `tikz_asset_base` | string or `null` | Prefix for generated TikZ URLs, such as `/tikz/`. |

Process-wide files can be supplied once:

```sh
mathpreview-cli convert \
  --config /path/to/integration.toml \
  --macros /path/to/integration-macros.tex \
  --mathjax-url https://example.test/mathjax/tex-svg.js
```

Config and macro layers have related but distinct cascades. Configuration uses
built-in defaults, global config, project config, process `--config` files,
then request config files. TeX macros use bundled compatibility definitions,
the document preamble and its referenced local preamble files, the global
override, the project override, process `--macros` files, then request macro
files. Markdown has no document-preamble step. Later layers win using the same
merge rules as `render` and `serve`. Missing prospective config and macro paths
are reported as dependencies with `exists: false`, allowing a host to watch for
their creation.

The request-level `options.mathjax_url` wins over process-level
`--mathjax-url`. With neither set, `convert` reports the jsDelivr MathJax 4
URL used by standalone rendering. The conversion process does not download
MathJax and does not serve the reported URL. A browser fetches that URL only
when a host builds and opens a viewer. If a host chooses a local URL such as
`/vendor/mathjax/tex-svg.js`, that host must serve it. The separate
`mathpreview-cli serve` command does serve MathPreview's embedded MathJax tree
at that path.

### Paths and limits

Paths are JSON strings. They are not promised to be canonical. A disk-backed
root or included file may resolve to an absolute canonical path, while a
logical unsaved path may remain exactly as supplied. Relative paths are
interpreted from the converter process's working directory. Consumers should
normalize paths before comparing them, but they should preserve the returned
spelling when displaying a source location.

The wire protocol can represent only UTF-8 paths. The in-process Rust API
retains native `PathBuf` values. If a wire result contains a non-UTF-8 path,
that request returns `conversion-failed` instead of writing a partial record or
ending the persistent stream.

The NDJSON boundary enforces these limits:

| Item | Limit |
| --- | --- |
| One input record | 64 MiB of JSON, excluding the line ending |
| Request `id` | 4 KiB of UTF-8 |
| Explicit config layers | 128 across process flags and request options |
| Explicit macro layers | 128 across process flags and request options |
| `file_overrides` entries | 1,024 |
| `source` plus override contents | Bounded by the 64 MiB record, including JSON overhead |

Blank input lines are ignored. There is no separate response-size limit. The
in-process Rust request allows the same 1,024 override entries, up to 256 MiB
of aggregate root and override text, and up to 2,048 caller-supplied dependency
records. These Rust limits do not enlarge the 64 MiB wire record.

### Successful response

A successful envelope has `status: "ok"` and a `result` object:

```jsonc
{
  "protocol": "mathpreview.converter/v1",
  "id": "edit-1",
  "status": "ok",
  "result": {
    "converter": {
      "id": "markdown",
      "api_version": 1,
      "format": "markdown",
      "extensions": ["md", "markdown"],
      "capabilities": {
        "buffer_source": true,
        "multi_buffer_source": false,
        "source_sync": true,
        "math_row_sync": false,
        "block_patching": true,
        "dependency_tracking": true,
        "asset_payloads": false
      }
    },
    "root_file": "notes.md",
    "body_html": "...",
    "blocks": [
      {
        "id": "blk-1",
        "hash": "...",
        "diff_hash": "...",
        "src": "notes.md:1:1",
        "source_anchors": [],
        "html": "...",
        "sub_blocks": null
      }
    ],
    "sync": { "entries": [], "math_rows": [] },
    "dependencies": [
      { "path": "notes.md", "kind": "root", "exists": false }
    ],
    "assets": [],
    "diagnostics": [],
    "runtime": {
      "title": "notes",
      "math": {
        "engine": "mathjax",
        "script_url": "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
        "macros": [],
        "packages": ["noerrors", "ams"],
        "loader_packages": ["[tex]/noerrors", "[tex]/ams"],
        "config": ""
      },
      "viewer": { "font_size": 18 }
    }
  }
}
```

The example abbreviates HTML, source anchors, sync entries, dependencies,
macros, and resolved viewer settings. A normal response also lists discovered
global and project config or macro paths, including prospective files that do
not exist yet. The actual response always includes the complete declared v1
fields.

Every response envelope contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `protocol` | string | Always `mathpreview.converter/v1`. |
| `id` | string or `null` | Parsed request ID. It is null when no valid ID of at most 4 KiB could be recovered. |
| `status` | string | `ok` or `error`. |
| `result` | object | Present only when `status` is `ok`. |
| `error` | object | Present only when `status` is `error`. |

### Result schema

The top-level `result` fields are:

| Field | Type | Meaning |
| --- | --- | --- |
| `converter` | object | Converter identity, source format, supported extensions, and capabilities. |
| `root_file` | path string | Resolved entry point or logical unsaved-buffer path. |
| `body_html` | string | Complete viewer body, without MathPreview's page shell. |
| `blocks` | array | Ordered patchable blocks described below. |
| `sync` | object | Structured source entries and multi-row math locations. |
| `dependencies` | array | Files that contributed to conversion or should be watched. |
| `assets` | array | Converter-owned generated-asset inputs. |
| `diagnostics` | array | Non-fatal structured conversion messages. |
| `runtime` | object | Math and viewer requirements needed to host the body. |

`body_html` and the per-block `html` strings deliberately carry the rendered
body in two forms. One supports an immediate full replacement, while the other
supports block patches without reparsing HTML. v1 always emits both. A future
response projection may let bandwidth-sensitive callers request only one.

`converter` contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `api_version` | integer | Converter artifact version. It is `1` for this contract. |
| `id` | string | Stable implementation ID, such as `latex` or `markdown`. |
| `format` | string | Open source-format name. |
| `extensions` | string array | Case-insensitive filename extensions without leading dots. |
| `capabilities` | object | Optional behaviors a viewer must check before using. |

The capability object contains seven booleans:

| Field | Meaning when `true` |
| --- | --- |
| `buffer_source` | The converter accepts an unsaved root in `source`. |
| `multi_buffer_source` | It accepts unsaved included, preamble, or bibliography files. |
| `source_sync` | It returns source anchors and structured sync entries. |
| `block_patching` | Its block IDs and diff hashes are suitable for live patching. |
| `math_row_sync` | It returns row locations for multi-row display math. |
| `dependency_tracking` | It returns typed conversion dependencies. |
| `asset_payloads` | It may return generated-asset source payloads. |

Each `blocks` entry contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Viewer element ID for this block. |
| `hash` | string | Opaque exact-block fingerprint used for DOM-state reuse. Compare only for equality. |
| `diff_hash` | string | Position-insensitive reconciliation hash. Treat it as opaque. |
| `src` | string or `null` | `data-src` value for the block's primary anchor. Generic clients should treat it as protocol data. MathPreview's current browser expects its source path, line, and column convention. |
| `source_anchors` | array | Additional `{ "id": string, "src": string }` anchors inside the block. |
| `html` | string | Complete HTML for the block. |
| `sub_blocks` | object or `null` | Optional fine-grained patch state for a known structured block. |

Each `source_anchors` entry has an `id` string naming the rendered element and
a `src` string suitable for its `data-src` attribute.

When present, `sub_blocks` contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `prefix_diff` | string | Opaque hash of the block scaffolding before its body. |
| `suffix_diff` | string | Opaque hash of the block scaffolding after its body. |
| `children` | array | Ordered independently patchable body children. |

Each child has a `diff_hash` string and an `html` string that expands to
exactly one top-level element. Hash algorithms and lengths are not part of the
contract. Compare hashes only as opaque strings.

The structured `sync` object is:

```jsonc
{
  "entries": [
    {
      "element_id": "srcw-g0-1",
      "file": "notes.md",
      "start": { "line": 1, "col": 1, "byte": 0 },
      "end": { "line": 1, "col": 6, "byte": 5 },
      "label": null,
      "kind": "leaf"
    }
  ],
  "math_rows": [
    {
      "element_id": "math-g0-2",
      "file": "paper.tex",
      "rows": [
        { "start_line": 10, "end_line": 10, "start_col": 3 }
      ]
    }
  ]
}
```

Each `sync.entries` item contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `element_id` | string | Rendered element ID. |
| `file` | path string | Source file containing the range. |
| `start` | position object | Inclusive start position. |
| `end` | position object | Exclusive end position. |
| `label` | string or `null` | Authored label or visible reference text when available. |
| `kind` | string | `leaf`, `container`, or `block`. |

Each position object has integer `line`, `col`, and `byte` fields. Each
`sync.math_rows` item has an `element_id` string, a `file` path string, and an
ordered `rows` array. Every row has integer `start_line`, `end_line`, and
`start_col` fields.

For `start` and `end`, `line` is 1-based, `col` is a 1-based UTF-8 byte column,
and `byte` is a 0-based UTF-8 byte offset in that file. These units match
Neovim's buffer API for both bundled converters. Entry ranges are half-open,
so `end` is the first position outside the element. A zero-width anchor may
have equal endpoints. `kind` is `leaf`, `container`, or `block`. `label` is an
authored label or visible lookup text when one is available. The `src` strings
in block metadata are for HTML attributes. Use these structured entries rather
than parsing `src` when exact source positions matter.

Each `math_rows` entry identifies one rendered multi-row math element. Row
`start_line` and `end_line` are inclusive and 1-based. `start_col` is the
1-based UTF-8 byte column of the first non-whitespace content. A value of `0`
means that the precise column is unknown, so a viewer should fall back to the
enclosing math block's anchor.

Each dependency has this shape:

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | path string | Input or prospective input path. |
| `kind` | string | `root`, `include`, `bibliography`, `config`, `macro`, or a converter-defined value. |
| `exists` | boolean | Whether that path existed on disk when the result was built. |

Each asset contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Converter-defined stable asset ID. |
| `kind` | string | Open asset kind. The bundled TeX converter uses `tikz-source`. |
| `payload_media_type` | string | Media type of `payload` itself. |
| `intended_output_media_type` | string or `null` | Expected type after a host processes the payload. |
| `encoding` | string | Open encoding name such as `json`, `utf-8`, or `base64`. |
| `payload` | any JSON value | Data interpreted according to `kind`, media type, and encoding. |

A bundled `tikz-source` asset uses `encoding: "json"`, payload media type
`application/vnd.mathpreview.tikz-source+json`, and intended output type
`image/svg+xml`. Its payload is
`{ "environment": string, "body": string, "preamble": string }`.
MathPreview does not compile it during conversion.

Each diagnostic contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `severity` | string | `info`, `warning`, or `error`. |
| `code` | string | Open machine-readable diagnostic code. |
| `message` | string | Human-readable explanation. |

A diagnostic belongs to an otherwise successful result. A request-level
failure uses the top-level error envelope instead.

The `runtime` object contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `title` | string | Resolved viewer title. |
| `math` | object or `null` | Browser math requirements, or null when none are needed. |
| `viewer` | object | Resolved viewer settings listed below. |

A non-null `math` object contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `engine` | string | Runtime ID. Bundled converters currently report `mathjax`. |
| `script_url` | string or `null` | Script URL a host may load. Conversion does not fetch it. |
| `macros` | array | Resolved macro definitions described below. |
| `packages` | string array | TeX package names for the engine's TeX input configuration. |
| `loader_packages` | string array | Loader package IDs such as `[tex]/ams`. |
| `config` | string | Trusted user JavaScript to apply to the math runtime. |

Each macro definition contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string | Command name without the leading backslash. |
| `body` | string | Resolved TeX replacement body. |
| `arguments` | integer | Number of accepted arguments. |
| `default` | string or `null` | Default for an optional first argument. |

The resolved `runtime.viewer` fields are:

| Field | Type | Meaning |
| --- | --- | --- |
| `font_size` | integer | Body font size in CSS pixels. |
| `ui_font_size` | integer | Viewer control font size in CSS pixels. |
| `hover_preview_scale` | integer | Hover preview scale as a percentage. |
| `default_page_mode` | string | `a4` or `dynamic`. |
| `default_theme` | string | `system`, `light`, or `dark`. |
| `source_jump_trigger` | string | `cmd-click`, `ctrl-click`, `alt-click`, or `double-click`. |
| `render_tikz` | boolean | Whether the viewer may request TikZ assets. |
| `theorem_numbering` | string | `auto`, `continuous`, or `section`. |
| `fancy_theorems` | boolean | Whether theorem presentation uses the decorated style. |
| `typeset_mode` | string | `local` or `background`. |
| `page_margin_mm` | number or `null` | Effective A4 page margin in millimetres. |
| `markdown_colon_fences` | boolean | Effective Markdown `:::` fence setting. |
| `keybindings` | object | Viewer action to an ordered array of key sequences. |
| `keybinding_aliases` | object | Key sequence to replacement sequence. |
| `key_sequence_timeout_ms` | integer | Timeout for an incomplete key sequence. |

Important result invariants:

- `blocks` is ordered and full-length. It is not a sparse patch. Each HTML
  value contains exactly one top-level `.blk` element with no nested `.blk`.
  Its outer ID, block hash, and source marker match its metadata.
- Treat block IDs, generated IDs, `src`, `hash`, `diff_hash`, and their
  algorithms as opaque protocol data. Equality of `hash` means the previous
  full block subtree is reusable except for the outer and source-anchor
  metadata the current client resynchronizes. Equality of `diff_hash` is the
  supported comparison for semantic block reconciliation. Known structured
  blocks may include `sub_blocks` for fine-grained patches.
- `body_html` is the complete document body. It is equivalent to concatenating
  the ordered block HTML and is convenient for an initial or full replacement.
- Every element named by `sync` or `source_anchors` exists in the returned
  HTML. Top-level block IDs and generated non-label IDs are unique. Consumers
  cannot assume authored label-derived IDs are unique because repeated TeX
  labels can produce collisions. Source anchors correspond one-to-one with
  their matching `[id][data-src]` descendants in DOM order.
- `sync` contains only public source entries and math-row mappings. Internal
  lookup caches are not serialized.
- `dependencies` contains the inputs a converter claiming
  `dependency_tracking` asks a host to watch. The bundled paths cover the root,
  includes, bibliographies, config, and macro files. `kind` is an open string
  for future converters.
- `assets` carries converter-owned payloads. The bundled LaTeX converter uses
  `kind: "tikz-source"`, JSON encoding, and payload media type
  `application/vnd.mathpreview.tikz-source+json`. Its intended output media
  type is `image/svg+xml`. Bundled conversion does not compile that
  trusted-project source.
- `runtime.math` carries resolved MathJax macros and package requirements, so
  custom LaTeX macros are not lost when the viewer owns the page shell. It also
  carries the script URL and may be `null` for a converter that needs no math
  runtime.
- `runtime.viewer` carries resolved display settings needed by that shell.
- `diagnostics` is structured and does not replace the top-level request error.

Capabilities are promises made by the converter. The host must not infer
support from a partially populated result. `validate_contract` checks the API
version and nonempty converter identity. `validate_against` also checks exact
metadata equality. These functions do not validate or sanitize HTML, hashes,
IDs, sync ranges, URLs, paths, assets, or the consistency between `body_html`
and `blocks`.

### Error response

Errors retain the protocol envelope and echo `id` when it was parsed as a
string within the 4 KiB limit:

```json
{
  "protocol": "mathpreview.converter/v1",
  "id": "edit-2",
  "status": "error",
  "error": {
    "code": "unsupported-converter",
    "message": "unknown converter \"pandoc\"; expected auto, latex, or markdown"
  }
}
```

v1 error codes are `invalid-json`, `invalid-request`, `request-too-large`,
`unsupported-protocol`, `unsupported-converter`, `configuration-failed`, and
`conversion-failed`.

The `error` object has a `code` string from that list and a human-readable
`message` string. Programs should branch on `code`, not parse `message`.

## Capability degradation

A viewer integrating another converter should preserve the document even when
optional metadata is unavailable:

- no `buffer_source`: require a saved root or reject the unsaved request
- no `multi_buffer_source`: save or reject child overrides instead of silently
  dropping them
- no `source_sync`: render normally, but disable forward/inverse source jumps
- no `math_row_sync`: fall back from individual rendered rows to the enclosing
  equation anchor
- no `block_patching`: replace the whole body rather than applying positional
  patches
- no `dependency_tracking`: watch only the root and explicit config, macro, and
  host inputs
- no `asset_payloads`: do not assume an asset payload exists or can be
  processed.

This list is normative guidance for a general host. The bundled live server
currently gates dependency watching and block patching explicitly. Some other
behavior still follows the closed LaTeX and Markdown format paths or the
presence of returned data.

## Versioning

The wire identifier and the core `CONVERTER_API_VERSION` both start at v1. A
breaking request, response, or capability-semantic change requires v2. Clients
should ignore unknown additive fields and must reject a protocol identifier
they do not implement.

This version is separate from `WS_PROTOCOL_VERSION`: the converter describes a
viewer-ready document, while WebSocket messages describe how one running
browser session is updated.

## Compatibility checks

Core regression tests compare the converter path with the legacy TeX and
Markdown entry points for both disk-backed documents and unsaved buffers. They
compare the public result and the internal diff hashes, sub-block state, and
TikZ payloads. Protocol tests cover version pairing, explicit converter
selection, runtime macros/packages, config and macro layering, structured
errors, oversized-record recovery, and the absence of private AST, preamble,
and sync-cache fields.

When extending the contract, run at minimum:

```sh
cargo test -p mathpreview-core converter::tests
cargo test -p mathpreview-cli convert::tests
```

## Trust and extension boundary

`mathpreview-cli convert` is a local API, not a sandbox. It reads paths with the
process's permissions. All converter-produced HTML is trusted active content
unless the host sanitizes it. `runtime.math.script_url` loads browser code,
configured text macros may intentionally emit HTML, and `runtime.math.config`
may contain trusted user JavaScript. `root_file`, dependency paths, and sync
paths can influence asset roots, file watches, and editor actions. A host must
not apply untrusted project config, execute an untrusted converter, or compile
returned TikZ payloads without an explicit trust decision. Built-in Markdown
raw HTML remains escaped and inert, but that guarantee does not apply to
arbitrary converter output.

The Rust trait is ready for implementations owned by a custom host, and the
wire result is AST-free and format-extensible, though viewer-oriented. The bundled
`mathpreview-cli convert` selector and `mathpreview-cli serve` currently know
only LaTeX and Markdown. They do not execute an arbitrary external converter
command from TOML or ingest an external artifact into the live server. A future
Pandoc, R Markdown, Quarto, or Typst adapter can target this contract, but it
must emit MathPreview's block, hash, source, and math DOM conventions for the
viewer features it claims. Automatic command registration also needs an
explicit process and trust policy.

The built-in live server consumes the same neutral document artifact in
process. It keeps the established preamble and bibliography cache plus a
private preamble and viewer sidecar around that boundary, so a keystroke does
not pay for NDJSON or a second render. Cached updates still use specialized
LaTeX and Markdown paths before converging on the shared artifact finalizer.
The stdio protocol is the stable cross-language boundary. The Rust trait is
the low-overhead boundary for in-process integrations.
