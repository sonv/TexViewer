# Converter and viewer architecture

This document records the converter API v1 architecture. It is the maintainer
guide for ownership, data flow, invariants, trust, and extension work. See
[Converter API v1](converter-api.md) for the normative Rust and NDJSON field
reference.

The central design decision is simple:

```text
source document -> converter -> viewer-oriented artifact -> host -> browser
```

The converter owns source-language interpretation. The host owns the live
preview application. They meet at `ConvertedDocument`, an AST-free snapshot
that contains body HTML and the metadata needed to display and update it.

This separation lets LaTeX and Markdown share the same downstream viewer
machinery. It also creates a stable target for future formats. It does not yet
make MathPreview a loadable converter plugin system. The stock CLI and live
server still select only the bundled LaTeX and Markdown converters.

## Architectural goals

We introduced this boundary to keep four concerns from growing together:

1. Source conversion should be replaceable without reimplementing the live
   server and browser.
2. The browser should receive HTML and update metadata, not a parser AST or a
   LaTeX preamble.
3. In-process Rust integrations and cross-language integrations should share
   one result shape.
4. The built-in live viewer should retain its low-latency caches while the
   public contract remains portable.

The fourth goal explains the private sidecar described below. The current
architecture establishes the public artifact boundary first while preserving
the optimized LaTeX and Markdown live paths.

## System map

There are three ways to reach the conversion boundary today:

```text
Custom Rust host
  -> host-selected DocumentConverter
  -> ConvertedDocument

NDJSON client
  -> mathpreview-cli convert
  -> static latex/markdown selector
  -> ConvertedDocument response

mathpreview-cli serve
  -> static BuiltinConverter adapter
  -> built-in conversion and cache path
  -> ConvertedDocument + private viewer sidecar
  -> watch, diff, WebSocket, and asset services
  -> MathPreview browser
```

These paths share the artifact. They do not share every orchestration detail.
In particular, the live server does not call the NDJSON command, and cached
live updates do not call `DocumentConverter::convert`.

## Ownership

| Layer | Owns | Does not own |
| --- | --- | --- |
| Integration or CLI host | Converter selection, config and macro-layer discovery, config merging, option assembly, request limits, trust decisions, invocation, and stale-request policy | Source parsing or browser DOM updates |
| Converter | Source and supplied macro-file loading, source-language macro interpretation, parsing, body HTML, ordered blocks, source maps, dependencies, assets, diagnostics, and runtime requirements | Page shell, file watching, WebSocket transport, or editor commands |
| Live server | Scheduling, caches, dependency watching, shell construction, asset processing, diff policy, server-forced reloads, HTTP, WebSockets, and source-navigation endpoints | Source-language syntax or browser-side patch application |
| Browser | Applying body and block updates, detecting pushed head-config reloads, controls, viewer-to-editor interactions, scroll state, and MathJax typesetting | Parsing source, computing server diffs, or watching files |

The browser receives a server-generated page shell. The server computes block
diffs and sends update instructions. Browser JavaScript applies those
instructions and retypesets affected math.

## The public boundary

The public Rust contract lives in
[`crates/core/src/converter.rs`](../crates/core/src/converter.rs). Its main
types are:

```text
ConversionRequest
  path
  optional unsaved root source
  optional unsaved project-file overrides
  caller-known dependencies

DocumentConverter
  metadata() -> ConverterMetadata
  supports_path(path) -> bool
  convert(request, &HtmlOptions) -> Result<ConvertedDocument>

ConvertedDocument
  converter metadata and capabilities
  root file
  body HTML
  ordered blocks
  public source-sync vectors
  dependencies
  generated asset payloads
  diagnostics
  runtime requirements
```

`DocumentConverter` is object-safe, synchronous, `Send`, and `Sync`. A custom
Rust host can store implementations behind `Box<dyn DocumentConverter>`.
That host is responsible for owning and selecting them.

The output is AST-free and format-extensible. It contains no LaTeX AST,
Markdown event stream, extracted preamble, or bibliography state. It is still
viewer-oriented. `ConvertedBlock` and the sync types reuse
MathPreview's renderer and sync wire shapes, and `convert` receives
`HtmlOptions`, which contains MathPreview concepts. Treat v1 as an AST-free
viewer artifact, not a universal HTML conversion interface.

### Artifact fields and consumers

| Field | Producer responsibility | Main consumer |
| --- | --- | --- |
| `converter` | Return the selected identity, format, extensions, and honest capabilities | Host validation and policy |
| `root_file` | Identify the logical or resolved root | Watcher and editor integration |
| `body_html` | Return the complete viewer body without the page shell | Initial render and full replacement |
| `blocks` | Return the complete ordered patch sequence | Live diff and browser patching |
| `sync` | Return structured source ranges and optional math rows | Forward and inverse navigation |
| `dependencies` | Return existing and prospective inputs with typed roles | File watcher |
| `assets` | Return typed source payloads, not compiled side effects | Host-defined asset handlers |
| `diagnostics` | Return non-fatal conversion messages | Embedding host |
| `runtime` | Return effective math and viewer requirements | Shell and reload policy |

### Artifact invariants

Capabilities are promises, not observations. A host should decide behavior
from the declared capability and then validate the corresponding data as
needed.

A valid v1 artifact follows these rules:

- `converter` must exactly match the metadata selected before conversion.
- `body_html` is the ordered concatenation of every `blocks[].html` value.
- `blocks` is a full, ordered snapshot. It is not a sparse patch.
- Each block HTML value contains exactly one top-level `.blk` element, with no
  nested `.blk`. Its `id`, `data-blockhash`, and `data-src` values match the
  block metadata.
- Block IDs, source-anchor IDs, generated IDs, `src`, `hash`, and `diff_hash`
  are protocol data. Consumers must not derive meaning from their spelling or
  hash algorithm.
- Equality of `hash` means the complete old block subtree is reusable except
  for the outer ID, class, block hash, source marker, and ordered descendant
  source-anchor metadata that the current full-body client resynchronizes.
- Equality of `diff_hash` is the supported test for semantic block reuse.
  It must change when semantic DOM content or scaffolding makes reuse unsafe.
  It may stay stable across source-position and generated-ID metadata changes
  that the patch client resynchronizes.
- Every element referenced by source sync or source-anchor metadata must exist
  in the returned HTML. Top-level block IDs and generated non-label IDs are
  unique. Consumers cannot assume authored label-derived IDs are unique because
  repeated TeX labels can produce collisions.
- `source_anchors` correspond one-to-one with their matching
  `[id][data-src]` descendants in DOM order because the current patch client
  updates them positionally.
- Source ranges are half-open. Lines and UTF-8 byte columns are 1-based, while
  UTF-8 byte offsets are 0-based.
- Each `sub_blocks.children[].html` value expands to exactly one top-level
  element. The current browser supports sub-block patching only for its known
  theorem, proof, callout, quote, and letter body conventions.
- Unknown dependency and asset kinds remain valid protocol values. A host must
  ignore or reject kinds it cannot process safely.

`ConvertedDocument::validate_contract()` checks the API version and nonempty
converter identity fields. `validate_against()` also checks exact metadata
equality. Neither function validates HTML structure, unique IDs, hashes, sync
ranges, paths, URLs, assets, or body and block consistency. A host accepting
third-party output must enforce the additional invariants it relies on.

`ConversionRequest::validate()` enforces the shared request limits, but the
trait cannot force a custom implementation to call it. Bundled converters do.
A custom host should validate before dispatch.

## Public artifact and private sidecars

The public artifact deliberately omits the page shell and parser-specific
state. The built-in viewer still needs a small amount of implementation-facing
state for compatibility and performance:

- `ConvertedSyncMap` serializes only `entries` and `math_rows`. An in-process
  conversion may retain a skipped label index. A deserialized artifact rebuilds
  that index once when it becomes a `SyncIndex`.
- `BuiltinConversion` carries `ConvertedDocument`, `ExtractedPreamble`, and
  the effective `HtmlOptions`. The live server uses this doc-hidden bridge to
  build the existing shell without parsing twice. It is technically public
  and re-exported for compatibility, but it is not part of the portable
  converter contract.
- `ViewerSidecar` in `serve.rs` holds the generated shell, closed
  `DocumentFormat`, and extracted preamble beside the neutral `LiveDocument`.
- `LegacyViewerSidecar` preserves the old full-page result when legacy output
  is projected into the neutral artifact for compatibility checks. It is also
  technically public, re-exported, and doc-hidden.

The public and private halves form one live snapshot:

```text
LiveSnapshot
  LiveDocument
    ConvertedDocument fields
    indexed SyncIndex, reusing the in-process cache or rebuilding after wire input
    recognized TikZ assets
  ViewerSidecar
    full page shell
    DocumentFormat
    ExtractedPreamble
```

This sidecar is an intermediate migration boundary. New source-language state
must not be added to `ConvertedDocument`. If the viewer genuinely needs new
portable information, model it as a general runtime requirement or a typed
asset. If only one built-in cache needs it, keep it behind the live adapter.

## Entry points

| Entry point | Conversion path | Result |
| --- | --- | --- |
| Rust `DocumentConverter::convert` | Public in-process contract | `ConvertedDocument` |
| `mathpreview-cli convert` | Persistent NDJSON, bundled selection, public trait | One complete artifact response per record |
| `mathpreview-cli serve` initial render | Built-in adapter and `convert_for_viewer` | `ConvertedDocument` plus private viewer state |
| `mathpreview-cli serve` later render | Cache-aware built-in LaTeX or Markdown path | Same artifact plus refreshed private viewer state |
| `mathpreview-cli render` | Legacy full-page renderer | Standalone HTML |
| `mathpreview-cli debug` | Legacy renderer diagnostics | Existing debug output |
| `render_document*` Rust functions | Legacy full-page renderer | `RenderOutput` |

The legacy entry points remain intentional compatibility surfaces. They do not
route through NDJSON, and `render` does not construct a `ConvertedDocument`.
Regression tests compare legacy and converter results so the migration does
not change established TeX or Markdown behavior.

## Built-in conversion flow

Both bundled converters validate the request and converge on
`finalize_builtin_document`.

### LaTeX

```text
resolve root or use buffer-backed root
  -> normalize unsaved file overrides
  -> load project includes
  -> extract preamble and macros
  -> load bibliography state
  -> build theorem registry
  -> parse and number body
  -> render body, blocks, sync, and TikZ inputs
  -> collect dependencies and runtime requirements
  -> finalize ConvertedDocument
```

LaTeX accepts unsaved root, include, preamble, and bibliography buffers. If a
caller is updating an included file, `request.path` must still identify the
project root and the child belongs in `file_overrides`.

### Markdown

```text
use root source or read root file
  -> apply effective macro and Markdown options
  -> parse Markdown events and custom blocks
  -> render through the shared body renderer
  -> collect root and host-supplied dependencies
  -> finalize ConvertedDocument
```

Markdown v1 is root-buffer only. It rejects unused non-root overrides instead
of silently pretending to support a multi-file project.

Display-math rows deliberately converge before rendering. The Markdown
frontend uses the parser-normalized event to prove and remove list or
blockquote prefixes, while retaining the exact authored TeX and recording
where each logical body line began in the source. The renderer then unwraps a
full outer equation-row environment such as `aligned` or `gathered` and sends
its interior through the same row splitter, copy-span builder, sync index,
live protocol, and browser row logic used by TeX. This keeps row semantics
format-neutral while leaving Markdown syntax recovery in the frontend that
owns it.

`finalize_builtin_document` is the shared seam. It moves the rendered blocks
and sync data into their public shapes, converts TikZ inputs to typed assets,
adds diagnostics, and derives the math and viewer runtime requirements.

## Persistent NDJSON conversion

[`crates/cli/src/convert.rs`](../crates/cli/src/convert.rs) exposes the
cross-language boundary as `mathpreview.converter/v1` on stdin and stdout.

```text
bounded line
  -> parse and validate request envelope
  -> select auto, latex, or markdown
  -> resolve config and macro layers
  -> build HtmlOptions and core ConversionRequest
  -> run bundled converter
  -> serialize response
  -> newline and immediate flush
```

The process is persistent to avoid startup cost. Conversion itself is
sequential, stateless, and full-snapshot:

- Requests are handled one at a time and responses preserve input order.
- `id` is correlation data only. It does not provide cancellation,
  deduplication, replay protection, or multiplexing.
- There is no incremental parser or preamble cache between records.
- One slow conversion delays records behind it. An editor host should debounce
  input and discard responses whose IDs are stale for that host.
- Blank lines produce no response.
- Malformed and oversized records return record-local errors when possible,
  then the process continues with the next record.
- A read failure, write or flush failure, panic, signal, or process exit ends
  the stream. The host should restart it.
- Stdout is reserved for NDJSON. Implementations must send logs to stderr.

Input records are bounded. Response size, asset payload size, block count,
conversion time, and converter memory are not bounded by the protocol. A host
running a future external process should add deadlines and output limits.

Supplying an unsaved `source` does not make a request hermetic. The logical
path still controls config and macro discovery, and LaTeX can read includes or
bibliographies that were not provided as overrides.

The stock command accepts only `auto`, `latex`, and `markdown`. It cannot
register an external executable or ingest a third-party artifact for the live
server.

## Live server flow

Initial startup selects the live adapter once from the original input path and
uses `BuiltinConverter::convert_for_viewer`. The adapter stays pinned for the
server lifetime. Cached renders must not redetect the format from a canonical
root because a Markdown-named symlink can target a file with another extension.
Later updates use the specialized cached path in
[`crates/cli/src/serve.rs`](../crates/cli/src/serve.rs):

```text
editor buffer push or watched-file event
  -> record newest source and coalesce render attempts
  -> acquire the single render permit
  -> run cached built-in LaTeX or Markdown conversion
  -> finalize ConvertedDocument
  -> combine it with a refreshed ViewerSidecar
  -> update dependency watches
  -> choose shell reload, body replacement, or block patch
  -> atomically commit snapshot and diff base
  -> broadcast WebSocket update
  -> browser applies update and typesets affected math
```

The single render permit prevents concurrent parses from multiplying memory.
Sequence checks collapse a burst of editor changes onto its newest stored
buffers. The body AST is rebuilt for each accepted update. The expensive
LaTeX preamble and bibliography state can be reused when its fingerprint has
not changed.

The cached path bypasses `DocumentConverter::convert` because the public trait
returns only the portable artifact. The server needs its preamble cache and
viewer sidecar without a second parse or render. Both built-in cached branches
still converge on `finalize_builtin_document`, so downstream watch, diff,
sync, and broadcast code consumes the same document shape.

### Live state and caches

The live host owns the mutable state needed to turn complete conversion
snapshots into a responsive editor session:

| State | Purpose | Invalidation or bound |
| --- | --- | --- |
| `buffer_overrides` and `buffer_push_seq` | Keep the newest unsaved root and child buffers per file | A matching disk save can remove an override |
| `render_seq` | Mark stale render attempts before they can commit | Monotonic for the server lifetime |
| `render_permit` | Allow only one expensive render at a time | At most one holder |
| `PreambleCache` | Reuse extracted preamble and bibliography state by fingerprints of preamble and bibliography inputs plus macro overrides | Fingerprint changes and watcher events clear or replace it |
| `file_content_cache` | Avoid rereading unchanged config and macro layers, including missing-file lookups | File existence or modification time changes |
| `last_blocks` | Keep the committed positional diff base | Replaced atomically with each broadcast snapshot |
| `tikz_cache` | Serialize TikZ jobs and retain successful SVG results | At most 32 successful entries, while failures are not retained |
| Temporary PDF preview cache | Retain content-hashed PNG previews produced by ImageMagick | Process-external disk cache under the system temporary directory, currently uncapped |

The body AST is not cached. Every accepted update reparses and rerenders the
body. Watcher events clear the preamble cache conservatively before the next
render.

### Update policy

The live system chooses the least disruptive safe update:

1. The server forces a full reload when effective runtime macros change.
2. The browser detects changes in pushed MathJax packages, MathJax config, and
   page margin, then reloads because those values live in the page head.
3. If `block_patching` is false, the server sends a full body replacement.
4. If a positional patch would cost roughly half the document or more, the
   server sends a full body replacement.
5. Otherwise the server sends a positional block patch with a full-length
   metadata synchronization vector. Each slot is `0` for unchanged metadata or
   a compact `{id, hash, src, anchors}` record. Patch operations carry changed
   HTML and sub-block data separately.

The live system does not reconcile every `runtime` field generically. A new
head-level requirement must define its comparison and reload behavior
explicitly.

The WebSocket metadata vector is full-length and positional. Block IDs can be
renumbered after insertion or deletion, so consumers must not treat them as a
permanent document identity.

Diff calculation, snapshot replacement, and broadcast-base replacement are
committed together. This keeps reconnects and later patches on one document
generation.

## Capabilities and degradation

The capability object tells a host which optional behavior it may use. A
general host should degrade as follows:

| Capability is false | Host behavior |
| --- | --- |
| `buffer_source` | Require a saved root or reject the unsaved request |
| `multi_buffer_source` | Save or reject child overrides rather than dropping them |
| `source_sync` | Disable both editor-to-viewer and viewer-to-editor navigation |
| `block_patching` | Replace `body_html` instead of sending positional patches |
| `math_row_sync` | Navigate to the enclosing equation anchor |
| `dependency_tracking` | Watch the root plus explicit config, macro, and host inputs |
| `asset_payloads` | Do not assume asset payloads are available or processable |

The bundled live server currently consults `dependency_tracking` and
`block_patching` explicitly. Other paths still use data presence or the closed
LaTeX and Markdown format enum. Before the server can host arbitrary
converters, buffer policy, navigation, math rows, and asset behavior need to
become capability-driven throughout.

## Viewer HTML dialect

`body_html` is not arbitrary standalone HTML. A full replacement can display
simple HTML, but MathPreview's advanced behavior depends on its DOM dialect:

- Patchable blocks are direct top-level `.blk` elements with matching block
  metadata and no nested `.blk` elements.
- Equal `hash` means the previous full block subtree is reusable except for
  the small set of outer and source-anchor metadata the client resynchronizes.
- `diff_hash` must represent semantic reuse correctly.
- Source anchors map one-to-one to real IDs and `data-src` attributes in DOM
  order.
- The current inverse-search client interprets `data-src` as a source path,
  line, and column even though generic consumers should treat the serialized
  string as protocol data.
- Structured sub-block patching recognizes MathPreview's theorem, proof,
  callout, quote, and letter containers.
- Math reuse and typesetting expect MathPreview's `.math[data-hash]`
  conventions. The bundled shell implements MathJax today.

A custom converter can omit optional patch, sub-block, sync, math-row, and
asset behavior by declaring honest capabilities. To use all current viewer
features, it must deliberately emit these conventions. The public artifact
does not translate arbitrary HTML into them.

## Dependencies, assets, diagnostics, and runtime

Converters report dependencies. Hosts decide whether and how to watch them.
The core built-ins include the root, TeX includes, bibliographies, macro files,
and caller-known inputs. The stock CLI and live server add resolved config
paths as caller-known dependencies. A missing prospective file remains a
dependency with `exists: false`, which lets the live server watch its parent
and react when the file is created.

Assets are typed, open-ended payloads. Their model separates converter output
from host-side asset processing, although the trait cannot constrain side
effects inside a custom converter implementation. The bundled LaTeX converter
emits `tikz-source` without compiling it. The live server recognizes that exact
payload and compiles it only through the established trusted-project path.
Unknown asset kinds are currently ignored by the stock server.

Diagnostics are structured and non-fatal. The NDJSON consumer receives them.
The live server currently retains them internally but does not present a
general diagnostics interface in the browser.

Runtime requirements report the effective math engine, script URL, macros,
packages, trusted configuration, and viewer settings. They let a host that
already supports the declared engine configure it around `body_html`. They do
not provide converter CSS, JavaScript, or a neutral runtime-to-shell builder. A
custom host must supply its own shell and runtime adapter. The current built-in
shell still uses the private preamble and `HtmlOptions` sidecar, so it does not
derive every shell decision solely from `runtime`.

Runtime changes may require a shell reload even when every body block patches
cleanly. Macros, packages, math configuration, scripts, and future head-level
styles must be part of the host's reload policy.

v1 has no converter-defined CSS or JavaScript bundle, no general asset-handler
registry, and no arbitrary shell-extension mechanism.

## Trust boundary

The conversion API is local, not sandboxed. Converter output is trusted input
to a viewer-controlled DOM.

A host must make explicit trust decisions for all of the following:

- HTML returned in `body_html` and `blocks[].html`
- `runtime.math.script_url` and trusted JavaScript configuration
- text macros that intentionally emit HTML
- `root_file`, dependency paths, and sync paths used for disk watches, asset
  requests, or editor actions
- generated assets such as TikZ source that can trigger external compilation
- any future external converter executable and its filesystem access

`validate_contract()` is not a sanitizer. Built-in Markdown escapes raw HTML,
but that guarantee does not extend to arbitrary converter implementations.
When the live server is bound to loopback, its Host and Origin checks defend
that network boundary. A non-loopback bind disables the loopback guard and
must be protected separately. Neither mode makes malicious same-origin
converted HTML safe.

An embedding host handling untrusted documents should sanitize HTML, restrict
URLs and paths, disable trusted runtime configuration, avoid compilation, and
isolate any converter process before displaying its output.

## Version boundaries

Four versions serve different purposes:

| Version | Protects | Bump when |
| --- | --- | --- |
| Package semantic version | Crates, CLI, and Neovim plugin release | Shipping a user-visible repository change |
| `CONVERTER_API_VERSION` | Meaning and shape of the core artifact contract | A breaking artifact or capability-semantic change |
| `mathpreview.converter/v1` | NDJSON framing, request, and response contract | A breaking cross-language protocol change |
| Browser `WS_PROTOCOL_VERSION` | Messages exchanged by one live server and tab | A WebSocket message shape or semantic change |

The core converter version and NDJSON identifier protect different surfaces,
but they form one lockstep converter compatibility epoch today. A breaking
request, response, artifact, or capability-semantic change must bump both.
Their numeric pairing is enforced by a regression test. They remain separate
from the browser WebSocket protocol. Adding a converter implementation does
not by itself require a WebSocket bump. Changing runtime behavior, shell
behavior, or browser messages may still require a reload or a WebSocket bump.

Additive JSON evolution must remain optional for old producers and ignorable
for old consumers. Rust artifact types marked `non_exhaustive` should be built
with their constructors and builders. Several aliased renderer and sync types
are still exhaustive Rust structs, so a Rust semver review is required even
when the JSON change appears additive.

## Adding another converter

There are two different extension scopes.

### Custom Rust host today

An external Rust application can:

1. Define stable metadata and honest capabilities.
2. Implement `DocumentConverter` and validate requests.
3. Produce a complete, internally consistent `ConvertedDocument`.
4. Register and select that converter in its own host.
5. Supply its own shell construction and an adapter for every runtime engine
   it supports.
6. Implement watching, update policy, asset handling, trust, and serving.

This does not register the converter with `mathpreview-cli convert` or
`mathpreview-cli serve`.

### Bundled MathPreview converter

To add a format to the stock product:

1. Add its metadata and `DocumentConverter` implementation in `converter.rs`.
2. Add static selection in `BuiltinConverter` and the CLI `select_converter`.
3. Decide whether the closed `DocumentFormat`, root resolution, config, macro,
   and legacy render entry points need to understand it.
4. Produce the MathPreview DOM dialect required by every claimed capability.
5. Add its live initial and cached conversion behavior.
6. Define shell requirements, full-reload triggers, dependency policy, and
   asset handlers.
7. Add editor and plugin extension detection when the stock plugin should open
   it automatically.
8. Add artifact, protocol, parity, failure, trust, source-sync, patching, and
   live-server tests.
9. Review the converter, NDJSON, Rust semver, and WebSocket version boundaries.
10. Update this architecture guide and the field reference.

A general external-converter feature needs additional host work. The preferred
direction is to construct the shell entirely from `ConvertedDocument.runtime`,
make buffer and navigation policy capability-driven, and add explicit
converter and asset-handler registration with a clear process trust policy.
Until then, do not describe v1 as an installable plugin interface.

## Current limits and non-goals

The following are intentionally not provided by v1:

- Dynamic converter registration in the stock CLI or TOML config
- Execution of an arbitrary external converter command by `serve`
- A route that feeds an external artifact into the stock live server
- Incremental, partial, or streaming conversion results
- Cancellation or concurrent multiplexing in the NDJSON process
- Converter-defined CSS, JavaScript, controls, or arbitrary page-shell hooks
- Automatic validation or sanitization of converter HTML
- General handling for asset kinds other than the built-in TikZ payload
- General presentation of converter diagnostics in the live browser

These limits are useful design constraints. They keep v1 small enough to
stabilize while making the remaining work visible.

## Code map

| File | Architectural role |
| --- | --- |
| [`crates/core/src/converter.rs`](../crates/core/src/converter.rs) | Public model and trait, bundled converters, artifact finalization, compatibility bridges |
| [`crates/core/src/lib.rs`](../crates/core/src/lib.rs) | Public exports and legacy full-page entry points |
| [`crates/core/src/renderer.rs`](../crates/core/src/renderer.rs) | Body and block HTML, hashes, anchors, and shell entry point |
| [`crates/core/src/renderer/shell.rs`](../crates/core/src/renderer/shell.rs) | Full MathPreview page shell |
| [`crates/core/src/sync.rs`](../crates/core/src/sync.rs) | Coordinate semantics and indexed source lookup |
| [`crates/cli/src/convert.rs`](../crates/cli/src/convert.rs) | NDJSON framing, limits, config resolution, and static converter selection |
| [`crates/cli/src/serve.rs`](../crates/cli/src/serve.rs) | Live adapter, sidecars, caches, watches, assets, diff policy, HTTP, and WebSockets |
| [`crates/core/src/assets/client/patch.js`](../crates/core/src/assets/client/patch.js) | Browser block and sub-block patch application |
| [`crates/core/src/assets/client/viewer.js`](../crates/core/src/assets/client/viewer.js) | Browser source interaction and viewer behavior |
| [`crates/core/src/assets/client/footer.js`](../crates/core/src/assets/client/footer.js) | Browser WebSocket client and protocol version |
| [`README.md`](../README.md) | User-facing entry points and installation guidance |
| [`docs/converter-api.md`](converter-api.md) | Normative converter request and result reference |

## Change checklist

When changing this architecture, verify the layer whose promise changed:

- Core artifact or bundled conversion:
  `cargo test -p mathpreview-core converter::tests`
- NDJSON framing, selection, or recovery:
  `cargo test -p mathpreview-cli convert::tests`
- Live conversion, caching, watching, sync, or patching:
  run the relevant `mathpreview-cli` serve tests and an end-to-end update
- Legacy compatibility:
  retain the converter versus `render_document*` parity tests
- Browser message shape or meaning:
  bump and pair both WebSocket protocol constants
- Any repository change:
  run the full workspace tests, Clippy, JavaScript lint, and Lua bytecode check

The most important review question is: did this change alter source
conversion, the portable artifact, host policy, or browser behavior? Keep the
answer visible in code, tests, and versioning instead of moving a concern
silently across the boundary.
