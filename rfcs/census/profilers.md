# A5 — What a Vyrn profiler has to be: a census

- **Status:** census. It designs nothing and decides nothing.
- **Date:** 2026-08-23.
- **Method:** one research pass per profiler, run independently; local reads of
  `compiler/`, `std/`, and `rfcs/`; one live experiment per machine-readable
  format (Part two). Sample provenance is stated where samples are used.
  Every claim carries a citation. Claims without one say `NOT FOUND` or
  `NOT MEASURED`.

---

## Part one — the five profilers

### JProfiler (ej-technologies)

| question | answer |
| --- | --- |
| Sampling, instrumenting, or both | Both. Method-call recording offers sampling (periodic call-stack inspection, default period typically 5 ms) and bytecode instrumentation (entry/exit tracing with invocation counts) (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/methodCallRecording.html) |
| Needs from the program | A native JVMTI agent loaded via `-agentpath`. No recompile, no source, no symbols (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/profiling.html) |
| Attach to running process | Yes, locally or over SSH, through the attach API; Docker/Kubernetes/OpenJ9 attach also supported (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/profiling.html) |
| Overhead as vendor states it | No single figure. "Very low overhead" for sampling; "near-zero overhead for full sampling when profiling Java 17+" (https://www.ej-technologies.com/news); instrumentation "can introduce a large overhead if many short-running methods are instrumented"; allocation recording defaults to every 10th allocation to cut overhead to roughly 1/10 (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/memory.html) |
| Beyond wall time | Allocation sites with stacks, lock contention with deadlock detection, thread states and dumps, GC telemetry, high-level probes for JDBC/JPA/HTTP/Kafka (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/memory.html, https://www.ej-technologies.com/resources/jprofiler/help/doc/main/threads.html). No cache misses, no system calls — the tool sees only what JVMTI exposes (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/profiling.html) |
| Output format named exactly | `.jps` snapshot; also opens/writes `.hprof`, `.phd`, `.jfr` (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/snapshots.html) |
| Text or binary; readable without the tool | Binary, compressed, undocumented. `bin/jpexport` exports view data to HTML/CSV/XML/SVG, so other programs consume exports rather than the snapshot (https://www.ej-technologies.com/resources/jprofiler/help/doc/commandLine/snapshotExecutables.html) |
| Presentation | Call tree, flame graphs (since JProfiler 12), hot-spot tables, sunburst diagrams, thread timelines, telemetry charts (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/cpu.html, https://www.ej-technologies.com/blog/2022/11/using-flame-graphs-when-profiling-java-applications/) |
| IDE integration | Deep plugins for IntelliJ IDEA, VS Code, Eclipse, NetBeans: profile from the IDE, embedded tool window, source navigation, inline CPU data in editors. An MCP server lets AI agents drive profiling (https://www.ej-technologies.com/resources/jprofiler/help/doc/main/mcp.html) |
| Cost and licence | Perpetual single-developer licence USD 549 (USD 768 with support), floating USD 2199; free licences for open-source projects (https://www.ej-technologies.com/store/jprofiler/new, https://www.ej-technologies.com/jprofiler/openSource) |

Better than the other four: JVM-aware whole-system analysis — probes, heap
walker, lock and deadlock analysis — that none of them offer
(https://www.ej-technologies.com/jprofiler).
Worse: closed binary snapshot format against pprof's open proto, no hardware
counters against VTune, and USD 549 against py-spy's MIT zero
(https://www.ej-technologies.com/resources/jprofiler/help/doc/main/snapshots.html).

### Intel VTune Profiler

| question | answer |
| --- | --- |
| Sampling, instrumenting, or both | Both. Two sampling collectors — user-mode sampling and tracing (OS timer per thread) and hardware event-based sampling (PMU counter overflow) — plus instrumentation/tracing APIs for JIT-style event annotation (https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/user-mode-sampling-and-tracing-collection.html, https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/hw-event-based-sampling-collection.html) |
| Needs from the program | No recompile for sampling; build "Release with debug info" for attribution (https://www.intel.com/content/www/us/en/docs/vtune-profiler/get-started-guide/2026-0/windows-os.html). Hardware EBS wants the Intel sampling driver installed, or Linux perf in driverless mode; root/sudo for system-wide collection (https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/hw-event-based-sampling-collection.html) |
| Attach to running process | Yes: `-target-process <name>` / `-target-pid <pid>`, GUI "Attach To Process" (https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/target-process.html) |
| Overhead as vendor states it | "The average overhead of event-based sampling is about 2% on a 1ms sampling interval." User-mode collector: "about 5% when sampling is using the default interval of 10ms" (the two collector pages above) |
| Beyond wall time | Cache misses and microarchitecture events (Microarchitecture Exploration, Memory Access analyses), memory consumption/allocations, threading and lock wait times, I/O, GPU/NPU offload, HPC characterization (https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/hw-event-based-sampling-collection.html, https://www.intel.com/content/www/us/en/developer/tools/oneapi/vtune-profiler.html) |
| Output format named exactly | Result files `*.vtune`, project files `*.vtuneproj`; CLI reports export to `.txt`/`.csv` via `-report ... -format csv -report-output f.csv` (https://www.intel.com/content/www/us/en/docs/vtune-profiler/get-started-guide/2026-0/windows-os.html, https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/report-output.html) |
| Text or binary; readable without the tool | Results are proprietary binary; only the exported text/CSV reports are readable elsewhere. No published schema for `.vtune` (`NOT FOUND`) |
| Presentation | GUI summary tables, bottom-up/top-down call trees, timeline, Flame Graph view ("Visualize hot code paths ... with Flame Graph", https://www.intel.com/content/www/us/en/developer/tools/oneapi/vtune-profiler.html) |
| IDE integration | Visual Studio toolbar integration out of the box; Visual Studio Code listed among supported development environments; standalone GUI and full CLI (https://www.intel.com/content/www/us/en/docs/vtune-profiler/get-started-guide/2026-0/windows-os.html, https://www.intel.com/content/www/us/en/developer/tools/oneapi/vtune-profiler.html) |
| Cost and licence | Stand-alone download at no charge (guest download, package managers, containers); licence is Intel's development-tools EULA linked from the download page (https://www.intel.com/content/www/us/en/developer/tools/oneapi/vtune-profiler-download.html) |

Better than the other four: hardware-counter truth — cache misses, memory
bandwidth, microarchitecture slots — plus GPU/NPU coverage none of the others
have (https://www.intel.com/content/www/us/en/docs/vtune-profiler/user-guide/2026-0/hw-event-based-sampling-collection.html).
Worse: heavy install with drivers and admin rights, proprietary result format,
and x86/Intel-centric measurement where py-spy attaches anywhere in seconds.

### py-spy

| question | answer |
| --- | --- |
| Sampling, instrumenting, or both | Sampling only; the FAQ positions itself against profilers that modify the profiled program (https://github.com/benfred/py-spy) |
| Needs from the program | Nothing: it reads interpreter state from another process's memory (`process_vm_readv`/`vm_read`/`ReadProcessMemory`). Debug symbols optional (BSS scan fallback); CPython 2.3–3.14, not PyPy; macOS needs root, Linux needs root or relaxed `ptrace_scope` when attaching by PID (https://github.com/benfred/py-spy#how-does-py-spy-work, https://github.com/benfred/py-spy#when-do-you-need-to-run-as-sudo) |
| Attach to running process | Yes — the headline use case: every subcommand takes `--pid`; "even if the program is serving production traffic" (https://github.com/benfred/py-spy#usage) |
| Overhead as vendor states it | No number. "Extremely low overhead ... safe to use against production Python code"; default rate 100 samples/second; `--nonblocking` avoids pausing the target (https://github.com/benfred/py-spy, https://github.com/benfred/py-spy/blob/master/src/config.rs) |
| Beyond wall time | Active-vs-idle thread detection, GIL holder detection (`%GIL`), native extension frames with `--native`, frame locals with `dump --locals`. No allocations, no lock events beyond GIL, no counters, no syscalls (https://github.com/benfred/py-spy#how-do-you-detect-if-a-thread-is-idle-or-not) |
| Output format named exactly | Four, from `--format`: `flamegraph` (.svg), `raw` (.txt, folded stacks), `speedscope` (.json), `chrometrace` (.json, undocumented in README but in source) (https://github.com/benfred/py-spy/blob/master/src/config.rs) |
| Text or binary; readable without the tool | All four text, all readable without py-spy: SVG opens in a browser, raw feeds flamegraph tooling, speedscope JSON opens in speedscope, chrometrace loads in trace viewers (https://github.com/benfred/py-spy#record) |
| Presentation | Interactive SVG flame graph, live terminal `top` table, point-in-time `dump` stacks (https://github.com/benfred/py-spy#record) |
| IDE integration | None first-party; integration is file-exchange level (https://github.com/benfred/py-spy#installation) |
| Cost and licence | Free, MIT (https://github.com/benfred/py-spy#license) |

`py-spy dump --json` detail: emits a pretty-printed JSON array of
`StackTrace { pid, thread_id, thread_name, os_thread_id, active, owns_gil,
frames[], process_info }` with frames `{ name, filename, module,
short_filename, line, locals, is_entry, is_shim_entry }`. The flag appears
nowhere in the README; its only documentation is the clap help string
"Format output as JSON" and the serde struct definitions. The shape is de
facto defined by Rust structs, with no version field and no stability promise
(https://github.com/benfred/py-spy/blob/master/src/dump.rs,
https://github.com/benfred/py-spy/blob/master/src/stack_trace.rs).

Better than the other four: zero-setup attach to an unmodified process, safe
enough for production, free (https://github.com/benfred/py-spy).
Worse: shallowest measurement of the five — sampled stacks and GIL flags only,
no allocations, counters, or syscalls like VTune, no aggregated interchange
format of its own like pprof (https://github.com/benfred/py-spy).

### JetBrains dotTrace

| question | answer |
| --- | --- |
| Sampling, instrumenting, or both | All three plus timeline: four session types — Sampling, Tracing (CLR enter/leave instrumentation), Line-by-Line (statement tracing, PDB required), Timeline (ETW). One type per session; attach supports only Sampling and Timeline (https://www.jetbrains.com/help/profiler/Basic_Concepts.html, https://www.jetbrains.com/help/profiler/Starting_Local_Profiling_Session.html) |
| Needs from the program | .NET Framework 1.0–4.8 / .NET Core / .NET 5–9 / Mono 5.10+ / Unity; PDB files for Line-by-Line; native symbol files for native frames in Timeline. No recompile (https://www.jetbrains.com/help/profiler/dotTrace_Introduction.html, https://www.jetbrains.com/help/profiler/Basic_Concepts.html) |
| Attach to running process | Yes; drag-and-drop Attach icon on Windows; Linux attach for .NET Core 3.0+, macOS for .NET 5+; no Mono/Unity attach (https://www.jetbrains.com/help/profiler/Starting_Local_Profiling_Session.html, https://www.jetbrains.com/help/profiler/dotTrace_Introduction.html) |
| Overhead as vendor states it | Qualitative only: Sampling/Timeline "Very lightweight"/"Lightweight"; Tracing "Heavyweight"; Line-by-Line "Extremely heavyweight". Samples taken with random 5–11 ms pauses. No percentages anywhere (https://www.jetbrains.com/help/profiler/Basic_Concepts.html) |
| Beyond wall time | Call counts (Tracing/Line-by-Line), memory allocation and GC events, file I/O, JIT compilation, lock contention and UI freezes (Timeline), SQL queries, Windows kernel calls in Timeline trees. No cache misses/hardware counters (https://www.jetbrains.com/help/profiler/Basic_Concepts.html, https://www.jetbrains.com/help/profiler/dotTrace_Whats_New.html) |
| Output format named exactly | `.dtp` snapshots (sampling/tracing/line-by-line), `.dtt` (timeline); imports `.nettrace`; `Reporter.exe` produces XML reports (https://www.jetbrains.com/help/profiler/Basic_Operations_with_Snapshots.html, https://www.jetbrains.com/help/profiler/Performance_Profiling__Profiling_Using_the_Command_Line.html) |
| Text or binary; readable without the tool | Snapshots binary and undocumented; the documented interchange is Reporter.exe's XML (text) and inbound open `.nettrace`. Another program cannot read a raw snapshot without dotTrace (same two pages) |
| Presentation | dotTrace Viewer: call tree, flame graph rendering of the tree, chronological timeline diagram, plain-list/hotspot tables acting as chained filters, per-line source view, snapshot comparison (https://www.jetbrains.com/help/profiler/Call_Tree.html, https://www.jetbrains.com/help/profiler/Timeline_Diagram.html) |
| IDE integration | Four forms: standalone app, command-line tool (also a .NET global tool), Visual Studio integration (ReSharper Performance Profiler window), Rider. Hot spot ↔ declaration navigation; TeamCity plugin; Rider 2026.2 adds a `dottrace-analyze` agent skill that analyzes snapshots into reports (https://www.jetbrains.com/help/profiler/dotTrace_Introduction.html, https://www.jetbrains.com/help/profiler/dotTrace_Whats_New.html) |
| Cost and licence | Sold inside dotUltimate only: personal USD 219 first year (renewals lower), commercial USD 609/user/year; All Products Pack USD 299/USD 979 first year; 30-day trial; no perpetual standalone SKU (https://www.jetbrains.com/profiler/buy/) |

Better than the other four: the deepest .NET workflow — ETW timeline with
GC/I/O/UI-freeze intervals, Rider/VS integration, per-line source mapping
(https://www.jetbrains.com/help/profiler/Basic_Concepts.html).
Worse: closed snapshot format against pprof, subscription-only pricing against
free tools, and no hardware counters against VTune
(https://www.jetbrains.com/help/profiler/Basic_Operations_with_Snapshots.html).

### pprof

| question | answer |
| --- | --- |
| Sampling, instrumenting, or both | Neither, in itself: an offline analyzer/visualizer that "reads a collection of profiling samples in profile.proto format". The format "is independent of the type of data being collected"; producers may sample or count (https://github.com/google/pprof/blob/main/README.md, https://github.com/google/pprof/blob/main/proto/README.md) |
| Needs from the program | Nothing running: profiles come from a local file or HTTP fetch; optional binary/binaries for address symbolization. Producers emit gzip-compressed `profile.proto` — canonically Go's `runtime/pprof` (https://github.com/google/pprof/blob/main/doc/README.md, https://github.com/golang/go/blob/master/src/runtime/pprof/pprof.go) |
| Attach to running process | No injection/attach. Live interaction is limited to fetching from an HTTP endpoint the program itself exposes (`pprof http://host/debug/pprof/profile?seconds=N`) (https://github.com/google/pprof/blob/main/doc/README.md) |
| Overhead | The tool adds zero — it runs outside the process on collected data. Producer cost model: Go's CPU profiler samples at a hard-coded 100 Hz, "frequent enough to produce useful data, rare enough not to bog down the system" (https://github.com/google/pprof/blob/main/doc/README.md, https://github.com/golang/go/blob/master/src/runtime/pprof/pprof.go) |
| Beyond wall time | Whatever the producer records: CPU nanoseconds, wall seconds, syscall counts, heap allocations and space, block/mutex contention, goroutine profiles; hardware events via Linux perf converted with perf_data_converter (https://github.com/google/pprof/blob/main/proto/profile.proto, https://golang.org/src/runtime/pprof/, https://github.com/google/pprof/blob/main/README.md) |
| Output format named exactly | Gzipped protocol buffer defined by `profile.proto` (proto3, package `perftools.profiles`): "On disk, it is represented as a gzip-compressed protocol buffer" (https://github.com/google/pprof/blob/main/proto/profile.proto, https://github.com/google/pprof/blob/main/proto/README.md) |
| Text or binary; readable without the tool | Binary on disk, fully specified in public proto. Independent readers exist — the repo ships its own encode/decode library, and any protobuf implementation can read/write it (https://github.com/google/pprof/blob/main/profile/profile.go) |
| Presentation | Interactive CLI shell; text `-top`/`-text`, `-tree`, `-peek`, `-traces`; DOT call graphs (svg/png/pdf); web UI with Graph, Flame graph, annotated Source and Disassembly views (https://github.com/google/pprof/blob/main/doc/README.md) |
| Editor integration | None shipped; IDEs integrate from outside (GoLand captures and analyzes Go profiles in-IDE) (https://blog.jetbrains.com/go/2026/07/16/goland-2026-2-is-now-available/) |
| Cost and licence | Free, Apache License 2.0; no release tags exist (https://github.com/google/pprof/blob/main/LICENSE, https://github.com/google/pprof/tags) |

Better than the other four: the open interchange format — a documented proto
any program can read, write, merge, and diff, with text reports a script or a
model can consume (https://github.com/google/pprof/blob/main/proto/profile.proto).
Worse: it collects nothing itself, has no attach, and its views are browser
pages rather than editor surfaces (https://github.com/google/pprof/blob/main/doc/README.md).

#### The `profile.proto` schema — enough for a Vyrn emitter

Source of truth:
https://raw.githubusercontent.com/google/pprof/main/proto/profile.proto
(`syntax = "proto3"; package perftools.profiles;`). Field names and numbers
below are verbatim. Format rules come from the schema comments and
https://github.com/google/pprof/blob/main/proto/README.md.

**message Profile**

| field | number | type | notes |
| --- | --- | --- | --- |
| `sample_type` | 1 | repeated ValueType | describes each value slot; CPU example `[["cpu","nanoseconds"]]`; heap `[["allocations","count"],["space","bytes"]]` |
| `sample` | 2 | repeated Sample | the recorded samples |
| `mapping` | 3 | repeated Mapping | address ranges to binaries |
| `location` | 4 | repeated Location | locations referenced by samples |
| `function` | 5 | repeated Function | functions referenced by locations |
| `string_table` | 6 | repeated string | common string table; `string_table[0] must always be ""` |
| `drop_frames` | 7 | int64 | string-table index; regex of frames to drop |
| `keep_frames` | 8 | int64 | string-table index; kept even if matching drop_frames |
| `time_nanos` | 9 | int64 | collection time, ns past epoch |
| `duration_nanos` | 10 | int64 | duration if meaningful |
| `period_type` | 11 | ValueType | kind of events between samples, e.g. `["cpu","cycles"]` |
| `period` | 12 | int64 | events between sampled occurrences |
| `comment` | 13 | repeated int64 | string-table indices |
| `default_sample_type` | 14 | int64 | preferred value type index |
| `doc_url` | 15 | int64 | string-table index |

**message ValueType**: `type` = 1 (string-table index, e.g. `"cpu"`),
`unit` = 2 (e.g. `"nanoseconds"`).

**message Sample**: `location_id` = 1 (repeated uint64, leaf first),
`value` = 2 (repeated int64, parallel to `sample_type`), `label` = 3
(repeated Label).

**message Label**: `key` = 1, `str` = 2, `num` = 3, `num_unit` = 4 — all
string-table indices except `num`; at most one of `str`/`num` per label;
keys prefixed `pprof::` reserved.

**message Mapping**: `id` = 1, `memory_start` = 2, `memory_limit` = 3,
`file_offset` = 4, `filename` = 5, `build_id` = 6, `has_functions` = 7,
`has_filenames` = 8, `has_line_numbers` = 9, `has_inline_frames` = 10.

**message Location**: `id` = 1, `mapping_id` = 2, `address` = 3,
`line` = 4 (repeated Line; multiple entries are an inline chain, last entry
is the caller), `is_folded` = 5.

**message Line**: `function_id` = 1, `line` = 2, `column` = 3.

**message Function**: `id` = 1, `name` = 2, `system_name` = 3, `filename` = 4,
`start_line` = 5 — all name/file fields string-table indices.

Emitter checklist, from the schema and the reader's validation rules
(https://github.com/google/pprof/blob/main/profile/profile.go):

1. IDs are unique nonzero, 1-based; id 0 is rejected.
2. `string_table[0]` must be the empty string.
3. No dangling references: every `location_id` resolves to a Location, every
   nonzero `mapping_id` to a Mapping, every `function_id` to a Function.
4. Every Sample's `value` list has exactly `len(sample_type)` entries.
5. On-disk bytes are gzip-compressed protobuf.
6. Store unsampled human-useful values; include `period` so original values
   are recoverable.
7. Current main does not enforce ordering, but a maximally compatible emitter
   writes mappings/functions/locations before the samples that reference them
   and sorts samples by their location-id sequences.

A 247-byte gzipped profile was hand-encoded against this table and decoded by
an independent reader during this job, which is the working proof that no
protobuf library is needed to target the format.

---

## Part two — how an agent reads a profile

### The formats

| format | documented | stable | typical size | consumers |
| --- | --- | --- | --- | --- |
| `pprof -top` / `-text` | Yes: report shapes fixed in-tree, `flat flat% sum% cum cum%` columns, header lines spelled out (https://github.com/google/pprof/blob/main/internal/report/report.go, https://github.com/google/pprof/blob/main/doc/README.md) | Yes; de-facto ecosystem standard | KBs after `-nodecount` trimming | scripts, CI logs, humans, models |
| `pprof -traces` | Yes: separator line, one location per line, per sample (https://github.com/google/pprof/blob/main/internal/report/report.go) | Yes | scales with sample count | same |
| `pprof -proto` output | Yes: gzip + profile.proto (https://github.com/google/pprof/blob/main/internal/driver/commands.go) | Yes | tens of KB to MBs | anything speaking protobuf |
| py-spy `dump --json` | Flag exists in CLI help only; key names are serde implementation details, no schema, no version (https://github.com/benfred/py-spy/blob/master/src/dump.rs, https://github.com/benfred/py-spy/blob/master/src/stack_trace.rs) | Unstable in practice; fields accrete (`is_entry` appeared for CPython 3.12 behavior) | KBs (instantaneous snapshot) | ad-hoc scripts |
| speedscope file format | Yes, twice over: normative TS types (https://github.com/jlfwong/speedscope/blob/main/src/lib/file-format-spec.ts) and a published draft-07 JSON Schema (https://www.speedscope.app/file-format-schema.json). `$schema` URI pins the version; `shared.frames` referenced by index; `sampled` and `evented` profile kinds; units enum `none/nanoseconds/microseconds/milliseconds/seconds/bytes` | Yes; additive changes annotated inline ("Added in 0.6.0") | hundreds of KB to MBs | speedscope viewer, py-spy, rbspy, stackprof, async-profiler exporters |
| Chrome DevTools `.cpuprofile` | Undocumented as a file format; fully inferable from CDP type `Profiler.Profile`: `nodes[]`, `startTime`/`endTime` (µs), `samples[]` (leaf node ids), `timeDeltas[]` (µs); nodes carry `callFrame`, `hitCount`, `children` (https://chromedevtools.github.io/devtools-protocol/tot/Profiler/) | De-facto stable, formally unguaranteed; no version field; some producers omit `samples`/`timeDeltas` | multi-MB common (~10,000 samples/sec per VS Code docs, https://code.visualstudio.com/docs/nodejs/profiling) | Chrome DevTools, VS Code built-in viewer, Node `--cpu-prof`, speedscope import |
| Firefox Profiler JSON | Yes, two layers: Gecko format doc (https://github.com/firefox-devtools/profiler/blob/main/docs-developer/gecko-profile-format.md) and processed format whose normative spec is TypeScript types (https://github.com/firefox-devtools/profiler/blob/main/docs-developer/processed-profile-format.md) | Explicitly versioned and actively changing: processed-format changelog through Version 70, automatic upgraders (https://github.com/firefox-devtools/profiler/blob/main/docs-developer/CHANGELOG-formats.md) | MBs to tens of MBs | profiler.firefox.com; external viewers convert |

### The experiment: can a model read 100 KB and name the hot function?

Method. Each format got a real sample where possible:

- `.cpuprofile`: generated live with the Chrome DevTools Protocol
  (`Profiler.start`/`Profiler.stop`) over a page whose known-hot function is
  `hotCollatz`. 69,259 bytes, 8 nodes.
- speedscope: written by `py-spy record --format speedscope --rate 200` against
  a busy Python process whose known-hot function is `collatz_steps`.
  15,512 bytes.
- py-spy dump: `py-spy dump --pid <pid> --json` against the same process.
  748 bytes.
- pprof text: rendered in the exact `-top` and `-traces` column shapes of
  https://github.com/google/pprof/blob/main/internal/report/report.go over a
  hand-built sample whose flat winner is `hotCollatz` (610 ms of 940 ms).
- pprof binary: the 247-byte gzipped `profile.proto` described above, given to
  the model base64-encoded.
- Firefox Profiler JSON: hand-built following
  `gecko-profile-format.md` (schema-tagged columnar tables, `stringTable`
  indices), known-hot function `hotCollatz` at ~60% of 400 samples. 21,867
  bytes. No authentic fixture was reachable (GitHub API rate limit), so this
  sample is synthetic and labeled as such.

Each sample went to a small fast model with one question: name the hot
function. Results:

| input | size | answer | correct |
| --- | --- | --- | --- |
| `.cpuprofile` JSON | 69 KB | `hotCollatz`, ~99% of samples | yes |
| speedscope JSON | 15 KB | `collatz_steps` (busy2.py line 7) | yes |
| py-spy `dump --json` | 748 B | innermost frame `collatz_steps` at busy2.py:5, active thread holds GIL | yes |
| `pprof -top` text | 5 lines | `hotCollatz` (610 ms flat, 64.89%) | yes |
| `pprof -traces` text | 9 lines | `hotCollatz`, largest weight | yes |
| gzipped protobuf (base64) | 332 B | refused to answer; proposed shelling out to decode | no — binary is opaque to a model |
| Firefox Gecko JSON | 21 KB | named `collatzSteps`, citing stack 1 — which is `hotCollatz`'s stack | no — wrong attribution |

Findings:

1. Flat, self-describing JSON (`.cpuprofile`, speedscope) and flat text
   (`pprof -top`) read reliably. The model computed shares, not just names.
2. Indirection-heavy table formats mislead: the Gecko sample's
   stackTable/frameTable/stringTable chain produced a confident wrong answer.
   An emitter choosing a format should weight this.
3. Binary formats need a decoder step before a model sees them; after decoding
   they become the text cases above.

---

## Part three — the Visual Studio Code side

### How stock VS Code presents a `.cpuprofile`

There is no `.cpuprofile` support in VS Code core. Two bundled extensions do
the work: `ms-vscode.js-debug` takes profiles (command
`extension.js-debug.startProfile`, title "Take Performance Profile"), saves
them (for example `.vscode/<name>.cpuprofile`) and opens them
(https://github.com/microsoft/vscode-js-debug). Viewing comes from
`ms-vscode.vscode-js-profile-table` (https://github.com/microsoft/vscode-js-profile-visualizer),
which registers custom editors:

- `jsProfileVisualizer.cpuprofile.table`, `priority: "default"`, selector
  `*.cpuprofile` — double-clicking a profile opens the table view.
- `jsProfileVisualizer.cpuprofile.flame`, `priority: "option"` — reachable
  through Reopen Editor With.
- A `profileCodeLensProvider` decorates source functions with time-spent
  lenses linking back into the open profile, and a Realtime Performance
  webview shows live charts during debug sessions
  (https://github.com/microsoft/vscode-js-profile-visualizer).

The template: capture command → saved file → custom-editor webview → CodeLens
annotations back in source.

### Custom editor API; no `vscode.ProfileEditor`

The mechanism is the public `customEditors` contribution point plus
`window.registerCustomEditorProvider`
(https://code.visualstudio.com/api/extension-guides/custom-editors,
https://code.visualstudio.com/api/references/contribution-points#contributes.customEditors).
For a read-only profile viewer, `CustomReadonlyEditorProvider` gives a webview
without edit/undo/save obligations
(https://code.visualstudio.com/api/references/vscode-api).

No first-class `vscode.ProfileEditor` exists. Searching current `@types/vscode`
finds only unrelated terminal profiles; the proposed-API directory contains no
profile-content proposal usable here
(https://github.com/microsoft/vscode/tree/main/src/vscode-dts). A Vyrn profile
viewer must be an ordinary readonly custom editor, exactly as Microsoft did
for JS profiles.

### What the Rust and Go extensions expose

- rust-analyzer: nothing user-facing. Its only "Profiling" setting is for
  profiling rust-analyzer itself, disabled in release builds; the user manual
  mentions profiling not at all (https://github.com/rust-lang/rust-analyzer).
- VS Code Go: test-centric and delegating. "Go Test: Profile" collects
  CPU/memory/mutex profiles and visualizes them by shelling out to
  `go tool pprof`; CodeLenses start tests and benchmarks. No in-editor viewer
  (https://github.com/golang/vscode-go/wiki/features).

So neither language extension offers a precedent beyond "lens starts capture,
external tool renders".

### CodeLens, and where Vyrn already builds lenses

A lens is a range plus at most one command; click runs that single registered
command with the lens's arguments; ranges should span one line; unresolved
lenses resolve lazily for visible lines only
(https://code.visualstudio.com/api/references/vscode-api#CodeLens). Lenses
cannot render results inline — results need the webview route.

Vyrn already uses all of this:

- The client builds lenses in `editor/vscode/extension.js:143-280`:
  "▶ Run" over each `fn main` (`extension.js:148-157`), "▶ Run test"/
  "▶ Run all tests" (`extension.js:165-192`), bench lenses "▶ Run bench" /
  "▶ Run all benches" over `bench "name"` blocks
  (`editor/vscode/extension.js:195-222`), route lenses from the server
  (`extension.js:234-248`), and the RFC-0064 dev-server lens
  (`extension.js:256-277`). The bench commands run `vyrn bench --name <name>`
  in a terminal (`extension.js:299-306`).
- The server side deliberately answers semantic questions instead of
  registering a LSP `code_lens_provider`: `compiler/vyrn-lsp/src/main.rs:643-654`
  documents `vyrn/isDevEntry`, and the comment at
  `compiler/vyrn-lsp/src/main.rs:649-654` states that every lens the editor
  shows is built in `extension.js` from such answers. Route facts render as
  lenses via `route_facts` (`compiler/vyrn-lsp/src/main.rs:3204-3206`).

Implication: the natural entry point for a Vyrn profiler is another lens over
`fn main`/bench blocks that invokes a capture command; the natural display is
a custom editor, because a lens cannot show a call tree.

---

## Part four — what Vyrn already has

### The benchmarking face

- `std/bench.vyrn` is the harness runtime. `benchMeasure`
  (`std/bench.vyrn:136-192`) implements the divan-simplified loop: ~50 ms
  warmup, iteration auto-scaling to ≥1 ms per sample, ≥31 samples and ≥500 ms
  capped at 2 s, then min/median/mean per-iteration nanoseconds.
  `BenchResult` is `{ name, minNs, medianNs, meanNs, samples, iters }`
  (`std/bench.vyrn:115-122`). `benchJson` locks the report schema
  `{ backend, opt, benches: [...] }` compact, integer-only, declaration order
  (`std/bench.vyrn:203-227`). `blackBox` keeps benched work alive.
- `vyrn bench` has three faces (RFC-0055 + RFC-0063;
  `compiler/vyrn-cli/src/main.rs:3558-3579`): native timing through a
  synthesized harness compiled by clang `-O2`; `--check`, deterministic
  single interpreter runs, the CI face; `--json` and
  `--compare <baseline> [--threshold]` for machines
  (`compiler/vyrn-cli/src/main.rs:15-18`).
- Bench blocks are a separate AST field never walked by run/build
  (`compiler/vyrn-frontend/src/ast.rs:64-71`), fenced out of shipped binaries
  (`compiler/vyrn-frontend/src/floor.rs:150-153`), checked like Unit-returning
  functions (`compiler/vyrn-frontend/src/checker.rs:1222-1226`,
  `1433-1454`), shown in the outline
  so only root benches run (`compiler/vyrn-frontend/src/loader.rs:1191-1196`).

What `vyrn bench --json` contains: exactly `backend`, `opt`, and per-bench
`name/minNs/medianNs/meanNs/samples/iters` (RFC-0063 §1,
`rfcs/RFC-0063-ci-benchmarks.md:18-22`, locked by the round-trip test in
`std/bench.vyrn:248-256`). It answers "how long", never "where".
### Instrumentation hooks today

Two, both depth-related:

1. `blackBox` lowers to an optimizer barrier in native codegen
   (`compiler/vyrn-codegen/src/lib.rs:9414-9418`), checker-gated to bench/test
   bodies (`compiler/vyrn-frontend/src/checker.rs:1652-1656`).
2. Every compiled function calls `@__vyrn_call_enter()` at entry and the
   matching exit, counting one frame of the shared call-depth budget at the
   CALLEE so all three engines agree (`compiler/vyrn-codegen/src/lib.rs:4452-4471`).
   The interpreter counts identically: `call_depth: Cell<u32>`
   (`compiler/vyrn-frontend/src/interp.rs:2405`), incremented and checked
   against `CALL_DEPTH_LIMIT = 1_000`
   (`compiler/vyrn-frontend/src/interp.rs:2821-2834`,
   `interp.rs:42`).

There is no per-instruction counter anywhere in the interpreter, and no
per-function counter — the only per-call counter is the shared depth budget,
not attributed to function identity.

### What CI already records

(`rfcs/RFC-0063-ci-benchmarks.md`):

- Blocking step: `vyrn bench --check` over every bench-bearing example
  (`rfcs/RFC-0063-ci-benchmarks.md:133-136`).
- Informational `bench` job on push to main: runs `--json` (uploaded as
  artifacts, the trend record) and `--compare` against `bench/baseline.json`;
  the amendment removed `continue-on-error` and made seeding a
  download-and-commit; threshold `BENCH_THRESHOLD` default 2.0
  (`rfcs/RFC-0063-ci-benchmarks.md:174-209`).
- The RFC-0104 corpus records full environment and verification per dataset —
  OS, CPU, clang/rustc/node/wasmtime versions with lock hashes, flags, N runs,
  median-of-runs method (`rfcs/bench-0104/results/2026-08-19.json:1-36`).
  The site charts it (`rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md:138-149`).

### Exists vs missing

Exists:

- Wall-time aggregates per named bench block, native, with stable JSON.
- Deterministic pass/trap checking of bench bodies.
- A call-depth counter shared byte-for-byte across interpreter, native, wasm.
- CI artifact history of timing JSON and an environment record.
- Editor lenses that can launch anything a CLI can do.

Missing, smallest piece first:

1. Attribution. Every existing number is keyed by bench-block name. Nothing
   says which function inside the block spent the time. That is the gap
   between a stopwatch and a profile.
2. A stack sampler or per-function counter in at least one engine.
3. An emitter to a standard format (pprof proto or speedscope JSON) so the
   output lands in existing viewers and model-readable text.

---

## Options for Vyrn

`RECOMMENDATION, NOT A DECISION`

Ranked. Each option states cost, what it measures, what it cannot.

### Option 1 — an interpreter profiler that emits a standard format

Add per-function self-time/call-count accumulation to the interpreter's call
path (where `call_depth` already lives,
`compiler/vyrn-frontend/src/interp.rs:2405`), behind a flag like
`vyrn run --profile`, and emit pprof proto or speedscope JSON.

- Build cost: smallest. The interpreter already walks calls with names and
  lines; the experiment proved a minimal proto encoder is ~250 lines with no
  dependencies, and std/json already exists for the speedscope path.
- Measures: inclusive/exclusive time and call counts per function for
  interpreted execution; deterministic-enough for CI; works on all platforms
  the interpreter runs on, including wasm via vyrn-play.
- Cannot: see optimized native code, inlining effects, or hardware behavior.
  Numbers describe the reference semantics, not the shipped binary — the same
  reason `vyrn bench` refuses to time the interpreter
  (`compiler/vyrn-cli/src/main.rs:3565-3566`).

Rank 1. Smallest build, immediate agent-readable call trees, zero risk to
backends' byte-parity because it touches no backend.

### Option 2 — compiler-inserted instrumentation, opt-in, all engines

Extend the `@__vyrn_call_enter` pattern
(`compiler/vyrn-codegen/src/lib.rs:4452-4471`) to per-function counters or
timestamps emitted under a flag, in interpreter, native, and wasm alike.

- Build cost: medium-large. Three engines must stay observably identical; the
  existing comment warns that any cycle/function selection must be computed
  identically in all three or the engines stop counting the same calls
  (`compiler/vyrn-codegen/src/lib.rs:4464-4470`). Timestamps add observer
  distortion; counters alone give call counts, not time.
- Measures: exact per-function call counts everywhere; with timestamps,
  inclusive time per engine, enabling cross-engine comparison — something none
  of the five surveyed profilers do.
- Cannot: cheaply give exclusive time without heavy distortion; changes
  generated code, so it must stay opt-in to keep shipped binaries untouched.

Rank 2. Unique capability (three-engine comparison), highest parity risk.

### Option 3 — external sampling of the native binary

Sample the running native program from outside (OS timer signal or a helper
thread) and write profiles the way Go's runtime does at 100 Hz.

- Build cost: largest. Platform collectors, unwinding through generated LLVM
  code, symbolization back to Vyrn names, a runtime shim per OS. This is what
  JProfiler, VTune, dotTrace each took years to harden.
- Measures: the truth about the optimized machine code users ship — the thing
  Option 1 cannot see.
- Cannot: run in CI deterministically; reach wasm; stay simple.

Rank 3 on cost/benefit today. Right once programs exist whose native-only
behavior matters enough to pay for it.

---

## The format question

Should Vyrn emit pprof, speedscope, or its own format? Evidence, no choice.

**For pprof:**
- Documented protocol buffer with a public schema; independent encoders/decoders
  exist in the repo itself (https://github.com/google/pprof/blob/main/proto/profile.proto,
  https://github.com/google/pprof/blob/main/profile/profile.go).
- Largest consumer ecosystem: Go tooling, perf converters, flame-graph UIs,
  merge/diff across profiles (https://github.com/google/pprof/blob/main/doc/README.md).
- This job hand-encoded a valid profile in ~250 dependency-free lines and
  decoded it independently; the barrier is real but low.
- Costs: gzip+varint machinery in the emitter; text output requires either
  bundling pprof-like reporting or telling users to install pprof for `-top`.

**For speedscope:**
- Published JSON Schema plus normative TS types; designed for additive
  evolution with `$schema` pinning
  (https://www.speedscope.app/file-format-schema.json,
  https://github.com/jlfwong/speedscope/blob/main/src/lib/file-format-spec.ts).
- Emittable from `std/json` alone — the same machinery RFC-0063 already chose
  for bench reports (`rfcs/RFC-0063-ci-benchmarks.md:86-98`).
- Free web viewer renders it; py-spy and rbspy already feed it, so it is a
  proven emitter target for sampling profilers
  (https://github.com/jlfwong/speedscope/blob/main/README.md).
- Costs: smaller ecosystem than pprof; no merge/diff story; viewer is a
  website, not a CLI.

**For its own format:**
- Full control, zero dependencies, and the project has precedent for locking
  small stable JSON schemas and testing round-trips
  (`std/bench.vyrn:203-256`).
- The py-spy lesson cuts against it: an unversioned, source-defined JSON shape
  became the least documented and least stable of the surveyed outputs
  (https://github.com/benfred/py-spy/blob/master/src/stack_trace.rs).
- A private format starts with no viewers, no consumers, and every future
  reader must be built by hand — the position pprof and speedscope each grew
  out of.

One measured fact bears directly on the choice: the model-readability
experiment showed flat JSON and flat text both work, while indirection-heavy
tables mislead. Both candidate formats sit on the readable side of that line;
what separates them is ecosystem size versus emission simplicity.
