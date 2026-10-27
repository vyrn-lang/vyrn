//! How `vyrn run`, `serve` and `test` run a program's own wasm, in this process
//! by the embedded wasmtime. The WASI host is `vyrn_genwasm::wasi`'s, with the
//! working directory as the ambient root: the setup of `wasmtime run --dir .
//! --env ..`. This file adds the terminal's refusal of an `extern` import (see
//! [`open`]), the check oracle's import, and the resident instance `serve`
//! answers on.

use std::io::Write;
use std::path::Path;
use vyrn_frontend::trap;
use vyrn_genwasm::wasi;
use vyrn_lower::lastrun::SiteCount;
use wasmtime::{Caller, Engine, Func, Global, Module, Store, Val};

/// What one run produced. The exit code is `proc_exit`'s argument, or 1 when
/// the module trapped.
pub struct Outcome {
    pub code: i32,
    /// Standard output if captured; otherwise empty and already written through.
    pub stdout: Vec<u8>,
    /// Standard error if captured; otherwise empty and already written through.
    pub stderr: Vec<u8>,
    pub meter: Option<Meter>,
    /// What the profile instrument counted, for a module built with it.
    pub counts: Option<Counts>,
    /// Each completion the host delivered to a waiting module, in order.
    pub wakes: Vec<Wake>,
}

/// One completion the host delivered through `__vyrn_wake`: the operation's id and its status,
/// 0 when it completed and 1 when the host cancelled it, and for a request that completed the
/// packed reply ([`crate::wire`]), which the payload is a String of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wake {
    pub id: i64,
    pub status: i32,
    pub reply: Option<Vec<u8>>,
}

/// What one function did: the calls it received and the operations its own body executed, as
/// wasmtime's fuel meter counts them. `name` is empty for a function the emitter did not name.
pub struct FnCount {
    pub name: String,
    pub calls: u64,
    pub ops: u64,
}

/// The profile instrument's counters, read from the guest's memory after `_start`
/// ([`vyrn_codegen::direct`]'s `Profile`). `sites[0]` holds the blocks made before any site
/// ran, with no function, line or verb.
pub struct Counts {
    pub sites: Vec<SiteCount>,
    /// Every function that ran, in the module's order. The instrument's own are not here, and
    /// the sum of `ops` is what the module executes without the instrument.
    pub fns: Vec<FnCount>,
    /// The most bytes live at once.
    pub peak: u32,
    /// Blocks and bytes still live at exit, the arena's retained chunks excluded.
    pub live_blocks: u32,
    pub live_bytes: u32,
}

#[derive(Default)]
pub struct Run {
    /// `argv[0]` is the program's name; the rest is `args()`.
    pub argv: Vec<String>,
    /// Keeps the guest's standard output in [`Outcome::stdout`]; `vyrn routes`
    /// reads its answer out of it.
    pub capture_stdout: bool,
    /// Keeps the guest's standard error in [`Outcome::stderr`]; the test
    /// harness reads a trap's message out of it.
    pub capture_stderr: bool,
    /// Times the phases into [`Outcome::meter`], for `vyrn run --profile`.
    pub meter: bool,
    /// Delivers each of a waiting module's timers without waiting out its deadline, so
    /// `vyrn test` waits on nothing; otherwise each waits for its deadline in real time.
    /// The order of wakes is the same either way ([`Waits`]).
    pub virtual_clock: bool,
    /// Delivers these completions in order instead of the clock's, each to an operation the
    /// module started: a replay of [`Outcome::wakes`], or a cancellation a test asks for.
    pub replay: Option<Vec<Wake>>,
    /// DER certificates an `https://` request trusts beside `webpki-root-certs`: a test harness's
    /// root for its own server. The `vyrn` binary sets none, so no program can add one.
    pub test_roots: Vec<Vec<u8>>,
}

/// Wall time per host phase of a run.
pub struct Meter {
    /// Cranelift compiling the module.
    pub translate: std::time::Duration,
    /// Linking, instantiating, and reading the memory export.
    pub instantiate: std::time::Duration,
    /// `_start`, from the call to the exit.
    pub run: std::time::Duration,
}

/// This host's state beside WASI's.
struct Extra {
    /// The check oracle's rows: line `k` of the module's `vyrn:sites` section is site `k`'s, and
    /// line 0 the table's address.
    checks: std::sync::Arc<Vec<String>>,
    waits: Waits,
}

type Host = wasi::Guest<Extra>;

/// The host operations a waiting module started and the host has not woken, and the clock
/// their deadlines read. The host owns the event loop ([`settle_waits`]); the module's run queue
/// orders the tasks.
///
/// The clock is logical: `now` is the deadline of the wake the module runs in, 0 before the
/// first, or the time the host began the call that runs ([`Waits::begin`]). A deadline is `now`
/// plus the sleep, so the time the module computes never moves a later deadline, and the order
/// of wakes is the same on every host and machine. A request's reply is delivered when it
/// arrives, and `now` becomes that time; on the virtual clock a request takes no time, so every
/// pending reply, in start order, comes before the next timer.
struct Waits {
    /// Whether a timer waits for its deadline in real time; `vyrn test` delivers it at once.
    wall: bool,
    /// When the module started, in real time.
    start: std::time::Instant,
    /// The logical clock, in milliseconds since `start`.
    now: u64,
    /// The id the next operation takes.
    next: i64,
    /// Each pending timer's deadline and id.
    due: Vec<(u64, i64)>,
    /// Each pending request's id, in start order.
    fetches: Vec<i64>,
    /// The requests in flight; a replay sends none.
    net: crate::fetch::Net,
    /// The completions still to replay, last first ([`Run::replay`]).
    replay: Option<Vec<Wake>>,
    /// Each completion delivered so far.
    delivered: Vec<Wake>,
    /// Each end of a task this host started, as `vyrn_wait.done` reported it, until read.
    ended: Vec<Ended>,
}

/// The end of a task the host started by calling a waiting export: the host's id for the task,
/// status 0 with the result word or 1 cancelled, as `vyrn_wait.done(task, status, result)`
/// delivers it.
struct Ended {
    task: i64,
    status: i32,
    result: i64,
}

impl Waits {
    fn new(run: &Run) -> Self {
        Waits {
            wall: !run.virtual_clock,
            start: std::time::Instant::now(),
            now: 0,
            next: 1,
            due: Vec::new(),
            fetches: Vec::new(),
            net: crate::fetch::Net::new(run.test_roots.clone()),
            replay: run.replay.clone().map(|mut r| {
                r.reverse();
                r
            }),
            delivered: Vec::new(),
            ended: Vec::new(),
        }
    }

    /// The real time since the start, in milliseconds, which a wall-clock timer waits out.
    fn real(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// The host begins a call: on the wall clock, its task's time starts at its arrival.
    fn begin(&mut self) {
        if self.wall {
            self.now = self.real();
        }
    }

    /// `vyrn_wait.sleep(ms)`: a timer due `ms` after `now`; a negative `ms` is due now.
    fn sleep(&mut self, ms: i64) -> i64 {
        let id = self.next;
        self.next += 1;
        let at = self.now.saturating_add(ms.max(0) as u64);
        self.due.push((at, id));
        id
    }

    /// `vyrn_wait.fetch(request)`: sends the packed request, unless this is a replay, which
    /// delivers the recorded reply instead.
    fn fetch(&mut self, request: Vec<u8>) -> i64 {
        let id = self.next;
        self.next += 1;
        self.fetches.push(id);
        if self.replay.is_none() {
            self.net.start(id, request);
        }
        id
    }

    /// `vyrn_wait.cancel(id)`: no wait waits for operation `id`, so its timer never fires and
    /// its request is aborted.
    fn cancel(&mut self, id: i64) {
        self.due.retain(|d| d.1 != id);
        self.fetches.retain(|&f| f != id);
        self.net.cancel(id);
    }

    /// How long until [`Waits::fire`] has a completion without waiting: zero for a replay, off
    /// the wall clock, and once a reply came; `None` when no timer is pending and no reply came.
    fn due_in(&mut self) -> Option<std::time::Duration> {
        let zero = std::time::Duration::ZERO;
        if let Some(r) = &self.replay {
            return (!r.is_empty()).then_some(zero);
        }
        if !self.fetches.is_empty() && (!self.wall || self.net.arrived()) {
            return Some(zero);
        }
        let at = self.due.iter().min()?.0;
        let left = if self.wall {
            at.saturating_sub(self.real())
        } else {
            0
        };
        Some(std::time::Duration::from_millis(left))
    }

    /// The next completion to deliver: the next one to replay, or else the earliest pending
    /// timer, ties by id, its deadline waited out on the wall clock. `now` moves to the
    /// deadline.
    fn fire(&mut self) -> Result<Option<Wake>, String> {
        if let Some(r) = &mut self.replay {
            let Some(w) = r.pop() else {
                return Ok(None);
            };
            if let Some(i) = self.due.iter().position(|d| d.1 == w.id) {
                self.now = self.due.remove(i).0;
            } else if let Some(i) = self.fetches.iter().position(|&f| f == w.id) {
                self.fetches.remove(i);
            } else {
                return Err(format!("a replayed wake names no pending wait: {}", w.id));
            }
            self.delivered.push(w.clone());
            return Ok(Some(w));
        }
        let timer = (0..self.due.len()).min_by_key(|&i| self.due[i]);
        if !self.fetches.is_empty() {
            let left = match timer {
                Some(i) if self.wall => Some(std::time::Duration::from_millis(
                    self.due[i].0.saturating_sub(self.real()),
                )),
                _ => None,
            };
            if let Some((id, reply)) = self.net.next(left, !self.wall) {
                self.fetches.retain(|&f| f != id);
                if self.wall {
                    self.now = self.real();
                }
                return Ok(Some(self.deliver(id, Some(reply))));
            }
        }
        let Some(i) = timer else {
            return Ok(None);
        };
        let (at, id) = self.due.remove(i);
        let real = self.real();
        if self.wall && at > real {
            std::thread::sleep(std::time::Duration::from_millis(at - real));
        }
        self.now = at;
        Ok(Some(self.deliver(id, None)))
    }

    /// Records the completion of operation `id`, with a request's reply.
    fn deliver(&mut self, id: i64, reply: Option<Vec<u8>>) -> Wake {
        let w = Wake {
            id,
            status: 0,
            reply,
        };
        self.delivered.push(w.clone());
        w
    }
}

/// The host's event loop around a module that waits: delivers each pending timer to
/// `__vyrn_wake` as it comes due, until none is pending. A module that exports no
/// `__vyrn_wake` waits for nothing. The error is the wake's, a `proc_exit` included.
fn settle_waits(store: &mut Store<Host>, inst: &wasmtime::Instance) -> wasmtime::Result<()> {
    let Ok(wake) = inst.get_typed_func::<(i64, i32, i64), i32>(&mut *store, WAKE) else {
        return Ok(());
    };
    loop {
        let Some(w) = store
            .data_mut()
            .x
            .waits
            .fire()
            .map_err(wasmtime::Error::msg)?
        else {
            return Ok(());
        };
        let payload = match &w.reply {
            Some(reply) if w.status == 0 => guest_string(store, inst, reply)?,
            _ => 0,
        };
        wake.call(&mut *store, (w.id, w.status, i64::from(payload)))?;
    }
}

/// Writes `bytes` into the guest as a String it owns: a block from its `__vyrn_malloc`, with
/// the `{ len, cap }` header and a NUL. Returns the String's address.
fn guest_string(
    store: &mut Store<Host>,
    inst: &wasmtime::Instance,
    bytes: &[u8],
) -> wasmtime::Result<i32> {
    let malloc = inst.get_typed_func::<i64, i32>(&mut *store, "__vyrn_malloc")?;
    let base = malloc.call(&mut *store, i64::from(STR_HDR) + bytes.len() as i64 + 1)?;
    let mem = (store.data().wasi.mem).ok_or_else(|| wasmtime::Error::msg("no memory"))?;
    place_string(mem.data_mut(&mut *store), base, bytes)
        .ok_or_else(|| wasmtime::Error::msg("the guest's memory is too small for the String"))
}

/// Lays `bytes` out as a String in the block at `base` of `data`: header, bytes, NUL. Returns
/// the String's address, eight bytes past the block's.
fn place_string(data: &mut [u8], base: i32, bytes: &[u8]) -> Option<i32> {
    let n = u32::try_from(bytes.len()).ok()?;
    wasi::wr32(data, base, n)?;
    wasi::wr32(data, base + 4, n)?;
    let at = (base + STR_HDR) as usize;
    data.get_mut(at..at + bytes.len())?.copy_from_slice(bytes);
    *data.get_mut(at + bytes.len())? = 0;
    Some(base + STR_HDR)
}

/// The export a waiting module takes each completion through.
const WAKE: &str = "__vyrn_wake";

use vyrn_codegen::direct::TRAPPED_EXPORT as TRAPPED;

/// One engine per process, and a second, metered one: fuel is a counter the
/// guest decrements in every block, so only a run under `VYRN_FUEL` pays for it.
fn engine(metered: bool) -> &'static Engine {
    static PLAIN: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    static METERED: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    let config = |fuel: bool| {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(fuel);
        cfg.max_wasm_stack(vyrn_frontend::trap::WASM_STACK_BYTES);
        // wasmtime refuses a wasm stack larger than the async one.
        cfg.async_stack_size(vyrn_frontend::trap::RUN_STACK_BYTES);
        Engine::new(&cfg).expect("the engine's configuration is fixed and valid")
    };
    if metered {
        METERED.get_or_init(|| config(true))
    } else {
        PLAIN.get_or_init(|| config(false))
    }
}

/// The wording of a trap the program did not spell: a stack overflow as
/// [`vyrn_frontend::trap::STACK_EXHAUSTED`], anything else as the engine's
/// first line.
fn host_trap(e: &wasmtime::Error) -> String {
    match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::StackOverflow) => vyrn_frontend::trap::STACK_EXHAUSTED.to_string(),
        _ => first_line(&format!("{e:?}")).to_string(),
    }
}

/// Hands a stack overflow back to the module: restores the stack pointer to
/// `top`, its value before the call, and calls the module's
/// [`vyrn_codegen::wasm::OVERFLOW_EXPORT`], which flushes standard output and
/// traps [`trap::STACK_EXHAUSTED`] as a check does. Returns that call's exit, or
/// `e` for any other trap.
///
/// An overflow is the engine's own, or the shadow stack's: an out-of-bounds
/// access with the stack pointer past the end of memory, where a frame push
/// that wrapped below 0 leaves it (`vyrn_codegen::wasm`). Call this before
/// anything else restores the stack pointer.
fn settle(
    e: wasmtime::Error,
    store: &mut Store<Host>,
    inst: &wasmtime::Instance,
    top: Option<i32>,
) -> wasmtime::Error {
    let sp = inst.get_global(&mut *store, vyrn_codegen::wasm::SP_EXPORT);
    let overflow = match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::StackOverflow) => true,
        Some(wasmtime::Trap::MemoryOutOfBounds) => {
            let at = sp.and_then(|g| g.get(&mut *store).i32());
            let end = store.data().wasi.mem.map(|m| m.data_size(&*store));
            matches!((at, end), (Some(at), Some(end)) if at as u32 as usize >= end)
        }
        _ => false,
    };
    if !overflow {
        return e;
    }
    let export = inst.get_typed_func::<(), ()>(&mut *store, vyrn_codegen::wasm::OVERFLOW_EXPORT);
    match (sp, top, export) {
        (Some(sp), Some(top), Ok(f)) if sp.set(&mut *store, top.into()).is_ok() => {
            f.call(&mut *store, ()).err().unwrap_or(e)
        }
        _ => wasmtime::Trap::StackOverflow.into(),
    }
}

/// The stack pointer's value, read before a call [`settle`] may need it for.
fn stack_top(store: &mut Store<Host>, inst: &wasmtime::Instance) -> Option<i32> {
    inst.get_global(&mut *store, vyrn_codegen::wasm::SP_EXPORT)
        .and_then(|g| g.get(&mut *store).i32())
}

/// Compiles and runs `bytes` as a WASI command.
///
/// `Err` is a failure of this host or of the module's shape: no `_start`, or a
/// trap that is not `proc_exit`. A program that traps on its own terms
/// (`error: ..` on fd 2, then `proc_exit(1)`) is `Ok` with code 1.
pub fn run(bytes: &[u8], run: Run) -> Result<Outcome, String> {
    let clock = std::time::Instant::now();
    let fuel_log = std::env::var_os("VYRN_FUEL");
    let module = compile(bytes, fuel_log.is_some())?;
    let (mut store, inst) = open(&module, &run, None)?;
    // Instantiating charges the module's data span, so `_start`'s own fuel starts here.
    let spent = |store: &Store<Host>| u64::MAX - store.get_fuel().unwrap_or(u64::MAX);
    let instantiated = spent(&store);
    let translate = module.translate;
    let instantiate = clock.elapsed().saturating_sub(translate);
    let start = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| format!("_start: {e}"))?;
    let clock = std::time::Instant::now();
    let top = stack_top(&mut store, &inst);
    let ran = (start.call(&mut store, ())).and_then(|()| settle_waits(&mut store, &inst));
    let code = match wasi::exit_code(ran.map_err(|e| settle(e, &mut store, &inst, top))) {
        Ok(code) => code,
        // A trap the program did not spell (`unreachable`, an out-of-bounds
        // access): the wording is this host's.
        Err(e) => {
            let msg = trap::line(&host_trap(&e));
            store.data_mut().wasi.write_err(msg.as_bytes());
            1
        }
    };
    let counts = (module.sites.as_ref())
        .and_then(|sites| read_counts(sites, module.fns.as_deref(), &inst, &mut store));
    let meter = run.meter.then(|| Meter {
        translate,
        instantiate,
        run: clock.elapsed(),
    });
    if let Some(log) = fuel_log {
        let fuel = spent(&store) - instantiated;
        log_line(
            Path::new(&log),
            &format!("{}\t{fuel}\n", run.argv.first().map_or("", |a| a.as_str())),
        )?;
    }
    let host = store.into_data();
    if let (vyrn_lower::check::Mode::Count(log), Some(counts)) =
        (vyrn_lower::check::mode(), &counts)
    {
        log_checks(log, run.argv.first().map_or("", |a| a.as_str()), counts)?;
    }
    Ok(Outcome {
        code,
        stdout: host.wasi.stdout.unwrap_or_default(),
        stderr: host.wasi.stderr.unwrap_or_default(),
        meter,
        counts,
        wakes: host.x.waits.delivered,
    })
}

/// Instantiates `module` under this process's environment and working
/// directory: everything before `_start`. Separate from [`run`] because a
/// [`Resident`] instance outlives `_start`.
fn open(
    module: &Compiled,
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Store<Host>, wasmtime::Instance), String> {
    let policy = wasi::Policy {
        ambient: Some(std::env::current_dir().map_err(|e| format!("cwd: {e}"))?),
        capture_stdout: run.capture_stdout,
        capture_stderr: run.capture_stderr,
        // A budget nothing exhausts: the counter is read, never a stop.
        fuel: module.metered.then_some(u64::MAX),
    };
    let extra = Extra {
        checks: module.checks.clone(),
        waits: Waits::new(run),
    };
    let gen = gen.unwrap_or_default();
    wasi::instantiate(&module.module, &policy, &run.argv, gen, extra, |linker| {
        // The host's waits ([`Waits`]).
        linker.func_wrap(
            vyrn_codegen::direct::WAIT_IMPORTS,
            "sleep",
            |mut c: Caller<'_, Host>, ms: i64| -> i64 { c.data_mut().x.waits.sleep(ms) },
        )?;
        linker.func_wrap(
            vyrn_codegen::direct::WAIT_IMPORTS,
            "fetch",
            |mut c: Caller<'_, Host>, at: i32, len: i64| -> wasmtime::Result<i64> {
                let mem = (c.data().wasi.mem).ok_or_else(|| wasmtime::Error::msg("no memory"))?;
                let request = (mem.data(&c).get(at as u32 as usize..))
                    .and_then(|d| d.get(..usize::try_from(len).ok()?))
                    .ok_or_else(|| wasmtime::Error::msg("a request outside memory"))?
                    .to_vec();
                Ok(c.data_mut().x.waits.fetch(request))
            },
        )?;
        linker.func_wrap(
            vyrn_codegen::direct::WAIT_IMPORTS,
            "cancel",
            |mut c: Caller<'_, Host>, id: i64| c.data_mut().x.waits.cancel(id),
        )?;
        linker.func_wrap(
            vyrn_codegen::direct::WAIT_IMPORTS,
            "done",
            |mut c: Caller<'_, Host>, task: i64, status: i32, result: i64| {
                let end = Ended {
                    task,
                    status,
                    result,
                };
                c.data_mut().x.waits.ended.push(end);
            },
        )?;
        // After `sweep`, a `vyrn` import is an `extern fn` the program reaches.
        // Only a browser page supplies that namespace. A terminal answers each
        // name with `trap::extern_unavailable`'s sentence on fd 2, then exit 1,
        // as the interpreter and `vyrn_codegen::toolchain::wasi_host_c` do, so a
        // reached `extern` fails the same way on every engine.
        for imp in module.module.imports() {
            if imp.module() != "vyrn" {
                continue;
            }
            let Some(ty) = imp.ty().func().cloned() else {
                continue;
            };
            let msg = trap::line(&trap::extern_unavailable(imp.name()));
            linker.func_new("vyrn", imp.name(), ty, move |mut caller, _, _| {
                caller.data_mut().wasi.write_err(msg.as_bytes());
                Err(wasi::Exit(1).into())
            })?;
        }
        // The check oracle's import ([`vyrn_lower::check::Mode::Count`]).
        linker.func_wrap(
            "vyrn_check",
            "fail",
            |mut c: Caller<'_, Host>, id: i32| -> wasmtime::Result<()> {
                let row = c
                    .data()
                    .x
                    .checks
                    .get(id as usize)
                    .map_or(String::new(), |r| r.replace('\t', " "));
                let msg = trap::line(&format!("{}: {row}", trap::PROVED_CHECK_FAILED));
                c.data_mut().wasi.write_err(msg.as_bytes());
                Err(wasi::Exit(1).into())
            },
        )?;
        Ok(())
    })
}

/// One module, translated once and instantiable many times: `--workers N`
/// costs one Cranelift compile and N instantiations.
pub struct Compiled {
    module: Module,
    metered: bool,
    translate: std::time::Duration,
    checks: std::sync::Arc<Vec<String>>,
    /// The `vyrn:sites` section: the counter table's address, then a row per site.
    sites: Option<String>,
    /// The `vyrn:fns` section: the counter table's address, then a name per function.
    fns: Option<String>,
    /// Each export that waits, by name, with its declared result kind ([`waiting_exports`]).
    waiting: std::sync::Arc<std::collections::HashMap<String, String>>,
}

/// Translates `bytes`; `metered` makes the guest count fuel ([`run`]).
pub fn compile(bytes: &[u8], metered: bool) -> Result<Compiled, String> {
    let clock = std::time::Instant::now();
    let sites = custom_section(bytes, "vyrn:sites");
    let module = Module::new(engine(metered), bytes).map_err(|e| {
        // A module the engine refuses is a compiler defect, and the bytes are
        // its only evidence (#444). The counter keeps two threads' modules apart.
        static KEPT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = KEPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let kept =
            std::env::temp_dir().join(format!("vyrn-invalid-{}-{n}.wasm", std::process::id()));
        match std::fs::write(&kept, bytes) {
            Ok(()) => format!(
                "wasm: {e:?}\n  note: the module is kept at {}",
                kept.display()
            ),
            Err(_) => format!("wasm: {e:?}"),
        }
    })?;
    Ok(Compiled {
        module,
        metered,
        translate: clock.elapsed(),
        checks: std::sync::Arc::new(
            sites
                .as_ref()
                .map_or_else(Vec::new, |c| c.split('\n').map(str::to_string).collect()),
        ),
        sites,
        fns: custom_section(bytes, "vyrn:fns"),
        waiting: std::sync::Arc::new(waiting_exports(bytes)),
    })
}

/// One instance that outlives `_start`, which `vyrn serve` answers requests on.
///
/// `proc_exit` unwinds the call and not the store, so the module's state
/// survives `main`. Every door is an `export extern fn` the CLI appended, with
/// the extern String ABI: an argument is a pointer to NUL-terminated UTF-8 the
/// caller allocates with `__vyrn_malloc` behind the `{ len, cap }` header, and
/// a returned String is the caller's to `__vyrn_free`. `web/wasi-min.js` does
/// the same marshalling.
pub struct Resident {
    store: Store<Host>,
    inst: wasmtime::Instance,
    /// The stack pointer and the nesting word's address, which a module with
    /// a door exports (`vyrn_codegen::wasm::Module::export_entry_state`).
    entry: Option<(Global, usize)>,
    waiting: std::sync::Arc<std::collections::HashMap<String, String>>,
    /// The id the next call this host begins takes.
    next: i64,
    /// Each task begun and not ended, by its id, with its export's declared result kind.
    open: std::collections::HashMap<i64, String>,
    /// Each end not yet taken, in the order it came ([`Resident::ended`]).
    ends: Vec<(i64, wasmtime::Result<Option<Val>>)>,
}

/// The eight-byte `{ len, cap }` header in front of every Vyrn String.
const STR_HDR: i32 = 8;

/// Compiles `bytes`, instantiates, and runs `_start`, leaving the store open.
/// A non-zero exit code from `main` aborts the serve, as under the interpreter.
pub fn start(
    bytes: &[u8],
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Resident, i32), String> {
    start_on(&compile(bytes, false)?, run, gen)
}

/// [`start`] on a module already translated: one worker of the pool.
pub fn start_on(
    module: &Compiled,
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Resident, i32), String> {
    let (mut store, inst) = open(module, run, gen)?;
    let entry = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| format!("_start: {e}"))?;
    let top = stack_top(&mut store, &inst);
    let ran = (entry.call(&mut store, ())).and_then(|()| settle_waits(&mut store, &inst));
    let ran = ran.map_err(|e| settle(e, &mut store, &inst, top));
    let code = wasi::exit_code(ran).map_err(|e| host_trap(&e))?;
    let sp = inst.get_global(&mut store, vyrn_codegen::wasm::SP_EXPORT);
    let nesting = inst
        .get_global(&mut store, vyrn_codegen::wasm::NESTING_EXPORT)
        .and_then(|g| g.get(&mut store).i32());
    let entry = sp.zip(nesting.map(|at| at as usize));
    let waiting = module.waiting.clone();
    Ok((
        Resident {
            store,
            inst,
            entry,
            waiting,
            next: 1,
            open: std::collections::HashMap::new(),
            ends: Vec::new(),
        },
        code,
    ))
}

impl Resident {
    /// One call of `f` into the instance. A trap abandons the frames and
    /// regions the call took, so after one this restores the stack pointer and
    /// the nesting word it read before, and in a module that waits hands the
    /// task that trapped to `__vyrn_trapped`, which forgets it and its waits.
    /// The error carries the host's id for that task, 0 for none. A trapped
    /// call's heap blocks stay allocated.
    fn enter(
        &mut self,
        f: Func,
        args: &[Val],
        out: &mut [Val],
    ) -> Result<(), (wasmtime::Error, i64)> {
        // The word lies in the statics, which memory always covers.
        let mem = self.store.data().wasi.mem;
        let saved = (self.entry).zip(mem).map(|((sp, at), mem)| {
            let mut nesting = [0u8; 4];
            nesting.copy_from_slice(&mem.data(&self.store)[at..at + 4]);
            (sp, sp.get(&mut self.store), at, mem, nesting)
        });
        let top = stack_top(&mut self.store, &self.inst);
        let ran = f.call(&mut self.store, args, out);
        let ran = ran.map_err(|e| settle(e, &mut self.store, &self.inst, top));
        let ran = ran.map_err(|e| {
            if let Some((sp, top, at, mem, nesting)) = saved {
                let _ = sp.set(&mut self.store, top);
                mem.data_mut(&mut self.store)[at..at + 4].copy_from_slice(&nesting);
            }
            let trapped = (self.inst).get_typed_func::<(), i64>(&mut self.store, TRAPPED);
            let host = trapped.and_then(|t| t.call(&mut self.store, ()));
            (e, host.unwrap_or(0))
        });
        // What `vyrn_wait.done` reported during the call.
        let ended = std::mem::take(&mut self.store.data_mut().x.waits.ended);
        for Ended {
            task,
            status,
            result,
        } in ended
        {
            let end = match (self.open.remove(&task), status) {
                (Some(kind), 0) => Ok(result_word(&kind, result)),
                (Some(_), _) => Err(wasmtime::Error::msg("the task was cancelled")),
                (None, _) => Err(wasmtime::Error::msg("a task this host never began ended")),
            };
            self.ends.push((task, end));
        }
        ran
    }

    /// Begins a call of export `door` with `args` under a new id, which it
    /// returns. An export that waits takes the id first and starts a task,
    /// which may end in this call or later; any other call ends now. Each end
    /// goes to [`Resident::ended`].
    pub fn begin(&mut self, door: &str, args: &[Val]) -> Result<i64, String> {
        let f = (self.inst)
            .get_func(&mut self.store, door)
            .ok_or_else(|| format!("{door}: no such export"))?;
        let id = self.next;
        self.next += 1;
        self.store.data_mut().x.waits.begin();
        let kind = self.waiting.get(door).cloned();
        let waits = kind.is_some();
        let args: Vec<Val> = (waits.then_some(Val::I64(id)).into_iter())
            .chain(args.iter().cloned())
            .collect();
        let mut out = vec![Val::I32(0); f.ty(&self.store).results().len()];
        if let Some(kind) = kind {
            self.open.insert(id, kind);
        }
        match self.enter(f, &args, &mut out) {
            Ok(()) if !waits => self.ends.push((id, Ok(out.first().cloned()))),
            Ok(()) => {}
            Err((e, _)) => {
                self.open.remove(&id);
                self.ends.push((id, Err(e)));
            }
        }
        Ok(id)
    }

    /// Delivers the next completion [`Waits::fire`] yields, waiting for it if
    /// it is not due; false when none is pending. A trap ends the task that
    /// trapped.
    fn wake_next(&mut self) -> Result<bool, String> {
        let Some(w) = self.store.data_mut().x.waits.fire()? else {
            return Ok(false);
        };
        let wake = (self.inst)
            .get_func(&mut self.store, WAKE)
            .ok_or_else(|| format!("{WAKE}: no such export"))?;
        let payload = match &w.reply {
            Some(reply) if w.status == 0 => self.alloc(reply)?,
            _ => 0,
        };
        let args = [
            Val::I64(w.id),
            Val::I32(w.status),
            Val::I64(i64::from(payload)),
        ];
        if let Err((e, host)) = self.enter(wake, &args, &mut [Val::I32(0)]) {
            self.open.remove(&host);
            self.ends.push((host, Err(e)));
        }
        Ok(true)
    }

    /// Calls `bell` on another thread whenever a reply to a request comes, so an
    /// event loop that waits for other events too, with [`Resident::advance`]'s
    /// time as its limit, wakes to deliver it. A request already in flight keeps
    /// the bell it started with.
    pub fn on_reply(&mut self, bell: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.store.data_mut().x.waits.net.on_reply = Some(bell);
    }

    /// Delivers each completion that is due, without waiting. Returns how long
    /// until the next one is due, `None` when no timer is pending and no reply
    /// came: a request in flight then wakes the loop through [`Resident::on_reply`].
    pub fn advance(&mut self) -> Result<Option<std::time::Duration>, String> {
        loop {
            match self.store.data_mut().x.waits.due_in() {
                Some(left) if left.is_zero() => self.wake_next()?,
                left => return Ok(left),
            };
        }
    }

    /// Each end since the last call, in the order it came: the id
    /// [`Resident::begin`] gave the call, and its result at the wasm type of the
    /// declared result, or the trap or cancellation that ended it. A trap no
    /// task this host began owns ends under id 0.
    pub fn ended(&mut self) -> Vec<(i64, wasmtime::Result<Option<Val>>)> {
        std::mem::take(&mut self.ends)
    }

    /// Calls export `name` with `args` and returns its result once it ended:
    /// an export that waits has every completion delivered first, each waited
    /// out. The outer error is a missing export or a call that never ended, the
    /// inner one a trap or a cancelled task.
    fn call(&mut self, name: &str, args: &[Val]) -> Result<wasmtime::Result<Option<Val>>, String> {
        let id = self.begin(name, args)?;
        if self.open.contains_key(&id) {
            while self.wake_next()? {}
        }
        let at = (self.ends.iter().position(|e| e.0 == id))
            .ok_or_else(|| format!("`{name}` never ended"))?;
        Ok(self.ends.remove(at).1)
    }

    /// Takes what the guest wrote to standard error since the last drain. A
    /// trap inside a door writes its `error: ..` line there before it exits.
    pub fn drain_err(&mut self) -> String {
        match &mut self.store.data_mut().wasi.stderr {
            Some(buf) => String::from_utf8_lossy(&std::mem::take(buf)).into_owned(),
            None => String::new(),
        }
    }

    /// Writes `bytes` into guest memory as a String: header, bytes, NUL. Returns
    /// the string's address; the block starts eight bytes before.
    fn alloc(&mut self, bytes: &[u8]) -> Result<i32, String> {
        let n = bytes.len() as i64;
        let base = self
            .call("__vyrn_malloc", &[Val::I64(i64::from(STR_HDR) + n + 1)])?
            .map_err(|e| format!("__vyrn_malloc: {e}"))?
            .and_then(|v| v.i32())
            .ok_or("__vyrn_malloc: no address")?;
        let mem = self
            .store
            .data()
            .wasi
            .mem
            .expect("memory is set before _start");
        place_string(mem.data_mut(&mut self.store), base, bytes)
            .ok_or_else(|| "the guest's memory is too small for the String".to_string())
    }

    /// Frees a String argument; the block starts at its header.
    fn release(&mut self, ptr: i32) -> Result<(), String> {
        self.call("__vyrn_free", &[Val::I32(ptr - STR_HDR)])?
            .map(|_| ())
            .map_err(|e| format!("__vyrn_free: {e}"))
    }

    /// Reads a returned String, its length from its header, then frees it: the
    /// result is the caller's.
    pub fn text(&mut self, ptr: i32) -> Result<String, String> {
        let mem = self
            .store
            .data()
            .wasi
            .mem
            .expect("memory is set before _start");
        let data = mem.data(&self.store);
        let at = ptr as usize;
        let bytes = (at.checked_sub(STR_HDR as usize))
            .and_then(|h| data.get(h..h + 4))
            .map(|n| u32::from_le_bytes([n[0], n[1], n[2], n[3]]) as usize)
            .and_then(|n| data.get(at..at + n))
            .ok_or_else(|| "a returned String lies outside memory".to_string())?;
        let out = String::from_utf8_lossy(bytes).into_owned();
        self.release(ptr)?;
        Ok(out)
    }

    /// The message for a trap out of a door: the guest's own `error: ..` line
    /// when it wrote one before `proc_exit`, else this host's wording.
    pub fn trapped(&mut self, door: &str, e: wasmtime::Error) -> String {
        match trap::split(&self.drain_err()).1 {
            Some(msg) => msg.to_string(),
            None => format!("{door}: {}", host_trap(&e)),
        }
    }

    /// Passes a door its String arguments and calls it for its effect.
    pub fn tell(&mut self, door: &str, args: &[&str]) -> Result<(), String> {
        let mut ptrs = Vec::with_capacity(args.len());
        for a in args {
            ptrs.push(self.alloc(a.as_bytes())?);
        }
        let vals: Vec<Val> = ptrs.iter().map(|p| Val::I32(*p)).collect();
        let out = self.call(door, &vals)?;
        // The caller owns a String argument, so it is freed even after a trap.
        for p in ptrs {
            let _ = self.release(p);
        }
        out.map(|_| ()).map_err(|e| self.trapped(door, e))
    }

    /// Calls a nullary door and returns what the guest wrote to standard error
    /// before its last `error: ..` line, and that line if it refused. A test
    /// body's output passes through whether the body passed or failed.
    pub fn call_body(&mut self, door: &str) -> (String, Option<String>) {
        let outcome = match self.call(door, &[]) {
            Err(e) => Err(e),
            Ok(r) => match wasi::exit_code(r.map(|_| ())) {
                Ok(0) => Ok(()),
                Ok(code) => Err(format!("exit {code}")),
                Err(e) => Err(host_trap(&e)),
            },
        };
        let said = self.drain_err();
        let Err(host) = outcome else {
            return (said, None);
        };
        let (out, msg) = trap::split(&said);
        (out.to_string(), Some(msg.map_or(host, str::to_string)))
    }

    /// Calls door `door` with `args` for its one result.
    fn ask(&mut self, door: &str, args: &[Val]) -> Result<Val, String> {
        match self.call(door, args)? {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(format!("{door}: no result")),
            Err(e) => Err(self.trapped(door, e)),
        }
    }

    pub fn ask_bool(&mut self, door: &str) -> Result<bool, String> {
        let v = (self.ask(door, &[])?.i32()).ok_or_else(|| format!("{door}: not a Bool"))?;
        Ok(v != 0)
    }

    /// A door that answers an `Int64`, called with `args`.
    pub fn ask_int(&mut self, door: &str, args: &[i64]) -> Result<i64, String> {
        let args: Vec<Val> = args.iter().map(|a| Val::I64(*a)).collect();
        (self.ask(door, &args)?.i64()).ok_or_else(|| format!("{door}: not an Int64"))
    }

    /// A door that answers a `String`, called with `args`.
    pub fn ask_text(&mut self, door: &str, args: &[i64]) -> Result<String, String> {
        let args: Vec<Val> = args.iter().map(|a| Val::I64(*a)).collect();
        let p = (self.ask(door, &args)?.i32()).ok_or_else(|| format!("{door}: not a String"))?;
        self.text(p)
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// The text of the custom section `name`, `None` for a module without one.
pub fn custom_section(bytes: &[u8], name: &str) -> Option<String> {
    section(bytes, name).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// A uleb128 at `at`, which it moves past.
fn leb(b: &[u8], at: &mut usize) -> usize {
    let (mut v, mut shift) = (0usize, 0);
    while let Some(&x) = b.get(*at) {
        *at += 1;
        v |= usize::from(x & 0x7f) << shift;
        shift += 7;
        if x & 0x80 == 0 {
            break;
        }
    }
    v
}

/// The payload of the custom section `name`. The walk trusts the bytes: the caller passes a
/// module the engine validated or the compiler wrote.
fn section<'b>(bytes: &'b [u8], name: &str) -> Option<&'b [u8]> {
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = leb(bytes, &mut at);
        let end = at + size;
        if id == 0 {
            let mut p = at;
            let n = leb(bytes, &mut p);
            if bytes.get(p..p + n) == Some(name.as_bytes()) {
                return bytes.get(p + n..end);
            }
        }
        at = end;
    }
    None
}

/// Each export the `vyrn:exports` section (`vyrn_codegen::direct::abi_section`) flags as
/// waiting, by name, with its declared result kind. Empty for a module without the section.
fn waiting_exports(bytes: &[u8]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(b) = section(bytes, "vyrn:exports") else {
        return out;
    };
    let mut at = 1;
    let text = |at: &mut usize| {
        let n = leb(b, at);
        let s = String::from_utf8_lossy(b.get(*at..*at + n).unwrap_or_default()).into_owned();
        *at += n;
        s
    };
    for _ in 0..leb(b, &mut at) {
        let name = text(&mut at);
        let ret = text(&mut at);
        for _ in 0..leb(b, &mut at) {
            text(&mut at);
        }
        if b.get(at) == Some(&1) {
            out.insert(name, ret);
        }
        at += 1;
    }
    out
}

/// A task's result word, as `vyrn_wait.done` delivers it, at the wasm type of the declared
/// result `kind`: an `i32` kind is the low half, a float its bits, `unit` none.
fn result_word(kind: &str, word: i64) -> Option<Val> {
    match kind {
        "unit" => None,
        "i64" | "u64" => Some(Val::I64(word)),
        "f64" => Some(Val::F64(word as u64)),
        "f32" => Some(Val::F32(word as u32)),
        _ => Some(Val::I32(word as i32)),
    }
}

/// Reads the counter tables out of the guest's memory after `_start`: `sites` and `fns` are the
/// module's `vyrn:sites` and `vyrn:fns` text; an oracle build has no `fns`. `None` when the
/// guest has no memory export or a table lies outside it.
fn read_counts(
    sites: &str,
    fns: Option<&str>,
    inst: &wasmtime::Instance,
    store: &mut Store<Host>,
) -> Option<Counts> {
    let mut lines = sites.split('\n');
    let table: usize = lines.next()?.parse().ok()?;
    let mem = inst.get_memory(&mut *store, "memory")?;
    let data = mem.data(&*store);
    let word = |at: usize, n: usize| -> Option<u64> {
        let b = data.get(at..at + n)?;
        Some(b.iter().rev().fold(0, |v, &x| v << 8 | u64::from(x)))
    };
    let mut out = Vec::new();
    for (k, row) in std::iter::once("").chain(lines).enumerate() {
        let mut f = row.splitn(3, '\t');
        let at = table + 16 + 32 * k;
        out.push(SiteCount {
            function: f.next()?.to_string(),
            line: f.next().and_then(|l| l.parse().ok()).unwrap_or(0),
            verb: f.next().unwrap_or_default().to_string(),
            blocks: word(at, 8)?,
            bytes: word(at + 8, 8)?,
            freed: word(at + 16, 8)?,
            live: word(at + 24, 8)?,
        });
    }
    let mut ran = Vec::new();
    if let Some(fns) = fns {
        let mut names = fns.split('\n');
        let rows: usize = names.next()?.parse().ok()?;
        for (k, name) in names.enumerate() {
            let (calls, ops) = (word(rows + 16 * k, 8)?, word(rows + 16 * k + 8, 8)?);
            if calls > 0 || ops > 0 {
                ran.push(FnCount {
                    name: name.to_string(),
                    calls,
                    ops,
                });
            }
        }
    }
    Some(Counts {
        sites: out,
        fns: ran,
        peak: word(table, 4)? as u32,
        live_blocks: word(table + 4, 4)? as u32,
        live_bytes: word(table + 8, 4)? as u32,
    })
}

/// Appends each check row's count to `log`: the program, then the row, tab-separated.
fn log_checks(log: &Path, program: &str, counts: &Counts) -> Result<(), String> {
    let mut out = String::new();
    for s in counts.sites.iter().skip(1) {
        let (function, line, label, n) = (&s.function, s.line, &s.verb, s.blocks);
        out.push_str(&format!("{program}\t{function}\t{line}\t{label}\t{n}\n"));
    }
    if out.is_empty() {
        return Ok(());
    }
    log_line(log, &out)
}

/// Appends `text` to the file `log`.
fn log_line(log: &Path, text: &str) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", log.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_module_is_kept_and_named() {
        let bad = b"\0asm\x01\0\0\0\x01";
        let Err(e) = compile(bad, false) else {
            panic!("a truncated section compiled");
        };
        let kept = e
            .split("kept at ")
            .nth(1)
            .expect("the error names the file");
        assert_eq!(std::fs::read(kept).expect("the file is there"), bad);
        let _ = std::fs::remove_file(kept);
    }

    #[test]
    fn two_refused_modules_are_kept_apart() {
        let kept = |bad: &[u8]| {
            let e = compile(bad, false)
                .err()
                .expect("a truncated module compiled");
            e.split("kept at ")
                .nth(1)
                .expect("the error names the file")
                .to_string()
        };
        let (a, b) = (b"\0asm\x01\0\0\0\x01", b"\0asm\x01\0\0\0\x02");
        let (at_a, at_b) = (kept(a), kept(b));
        assert_eq!(std::fs::read(&at_a).unwrap(), a);
        assert_eq!(std::fs::read(&at_b).unwrap(), b);
        let _ = (std::fs::remove_file(at_a), std::fs::remove_file(at_b));
    }

    /// A program with module state and an exported door.
    const PROBE: &str = r#"let mut hits = 0

export extern fn bump() -> Int64 {
    hits = hits + 1
    return hits
}

fn main() -> Int64 {
    hits = 100
    return 0
}
"#;

    // A compiled program calls `std/runtime`, which the loader injects from
    // the std root; this library target has no driver to find it on disk, so
    // the needed files are embedded.
    fn probe_bytes() -> Vec<u8> {
        let files = vyrn_frontend::loader::MapResolver(
            [
                (
                    "std/runtime.vyrn".to_string(),
                    include_str!("../../../std/runtime.vyrn").to_string(),
                ),
                (
                    "std/mem.vyrn".to_string(),
                    include_str!("../../../std/mem.vyrn").to_string(),
                ),
                (
                    "std/text.vyrn".to_string(),
                    include_str!("../../../std/text.vyrn").to_string(),
                ),
                (
                    "std/strpred.vyrn".to_string(),
                    include_str!("../../../std/strpred.vyrn").to_string(),
                ),
                (
                    "std/num.vyrn".to_string(),
                    include_str!("../../../std/num.vyrn").to_string(),
                ),
                (
                    "std/codecs.vyrn".to_string(),
                    include_str!("../../../std/codecs.vyrn").to_string(),
                ),
                (
                    "std/hash.vyrn".to_string(),
                    include_str!("../../../std/hash.vyrn").to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some("std".into()),
            expansions: vyrn_frontend::project::Expansions::shared(),
            ..Default::default()
        };
        let mut program =
            vyrn_lower::load(PROBE, "probe.vyrn", &opts, &files, None).expect("the probe loads");
        let diags = vyrn_lower::check_and_synthesize(&mut program, None);
        assert!(diags.is_empty(), "the probe checks: {diags:?}");
        let world = vyrn_lower::analyze(&program);
        vyrn_codegen::direct::compile(&program, world).expect("the probe compiles")
    }

    fn quiet() -> Run {
        Run {
            argv: vec!["probe.vyrn".to_string()],
            capture_stdout: true,
            capture_stderr: true,
            ..Run::default()
        }
    }

    /// Prints the resident and fresh per-answer costs with `--nocapture`; asserts
    /// only the order, because other worktrees' gates share the machine.
    #[test]
    fn a_resident_answer_is_cheaper_than_a_fresh_instance() {
        let bytes = probe_bytes();
        let run = quiet();
        let (mut store, inst) =
            open(&compile(&bytes, false).expect("compile"), &run, None).expect("open");
        let start = inst
            .get_typed_func::<(), ()>(&mut store, "_start")
            .expect("_start");
        let _ = start.call(&mut store, ());
        let bump = inst.get_typed_func::<(), i64>(&mut store, "bump").unwrap();
        let n = 1000;
        let clock = std::time::Instant::now();
        for _ in 0..n {
            bump.call(&mut store, ()).expect("resident call");
        }
        let resident = clock.elapsed() / n;

        let m = 20;
        let clock = std::time::Instant::now();
        for _ in 0..m {
            let (mut s, i) =
                open(&compile(&bytes, false).expect("compile"), &run, None).expect("open");
            let start = i.get_typed_func::<(), ()>(&mut s, "_start").unwrap();
            let _ = start.call(&mut s, ());
            i.get_typed_func::<(), i64>(&mut s, "bump")
                .unwrap()
                .call(&mut s, ())
                .expect("fresh call");
        }
        let fresh = clock.elapsed() / m;
        eprintln!("resident {resident:?} per answer, fresh {fresh:?} per answer");
        assert!(resident < fresh, "resident {resident:?}, fresh {fresh:?}");
    }
}
