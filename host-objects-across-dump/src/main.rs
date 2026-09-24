//! Experiment I — host class instances held in long-lived guest state.
//!
//! The handoff's warning, listed as untested: *"the host-side class-instance
//! store is **not** part of a dump. Restoring into a fresh process starts with
//! an empty store, so returned host objects become proxies and method calls on
//! them raise. Keep host objects out of long-lived guest state."*
//!
//! That prohibition is expensive if true. Handing the guest a host object — a
//! ledger, a document, a connection handle — is the natural way to expose
//! stateful capabilities, and a durable runtime keeps guest state across
//! hibernations by definition. So the question is not only *does it break* but
//! **can the host put it back**.
//!
//! Experiment B established the analogous answer for external *functions*: the
//! dump records the name, never the implementation, so a fresh process resolves
//! the same name to different code. Host objects route by `MontyUuid` instead of
//! by name. If resolution is likewise at call time, the same discipline should
//! apply: the host must be able to reconstruct an object under its old id.
//!
//! Every stage is a separate OS process (`./run.sh`), so the host store is
//! genuinely empty on the far side unless a stage deliberately refills it.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use monty::{Dump, MontyRepl, ReplProgress, Session, SessionRef, dump};
use monty_types::{
    CompileOptions, DictPairs, ExcType, ExtFunctionResult, MontyClassInstance, MontyClassType, MontyException,
    MontyObject, MontyUuid, NameLookupResult, PrintWriter, ResourceTracker,
};

const SCRIPT: &str = "agent-run.py";

/// Deterministic ids so a later process can deliberately rebind the same
/// instance. A real host would use uuid4 and persist the mapping beside the
/// snapshot — which is the whole point of the finding.
const LEDGER_CLASS_ID: MontyUuid = MontyUuid::from_u128(0x1ed6_e700_0000_0000_0000_0000_0000_0001);
const LEDGER_INSTANCE_ID: MontyUuid = MontyUuid::from_u128(0x1ed6_e700_0000_0000_0000_0000_0000_00a1);

fn quiet() -> PrintWriter<'static> {
    PrintWriter::Disabled
}

fn new_repl() -> MontyRepl {
    MontyRepl::new(SCRIPT, ResourceTracker::default(), CompileOptions::default())
}

fn exc_line(e: &MontyException) -> String {
    e.to_string().lines().next_back().unwrap_or_default().trim().to_owned()
}

/// The host object the guest gets a handle to. Never serialized: this is
/// exactly the thing the dump does not contain.
#[derive(Debug, Clone)]
struct Ledger {
    owner: String,
    balance: i64,
    limit: i64,
}

/// The host-side store the handoff warns about — instance id to live object.
#[derive(Debug, Default)]
struct HostStore {
    ledgers: HashMap<MontyUuid, Ledger>,
}

impl HostStore {
    fn insert(&mut self, id: MontyUuid, ledger: Ledger) {
        self.ledgers.insert(id, ledger);
    }

    /// Build the wire form of a handle to `id`. `attrs` here are the *eager*
    /// attributes: they cross with the instance and so end up inside the dump.
    fn handle(&self, id: MontyUuid) -> MontyObject {
        let owner = self.ledgers.get(&id).map_or("unknown", |l| l.owner.as_str());
        MontyObject::ClassInstance(Box::new(MontyClassInstance {
            class_type: MontyClassType {
                name: "Ledger".to_owned(),
                id: LEDGER_CLASS_ID,
                host_defined: true,
                is_dataclass: false,
                attrs: DictPairs::default(),
            },
            instance_id: id,
            attrs: DictPairs::from_iter([(MontyObject::String("owner".into()), MontyObject::String(owner.into()))]),
        }))
    }

    /// Answer a method call routed to an instance id.
    fn call_method(&mut self, id: MontyUuid, method: &str, args: &[MontyObject]) -> ExtFunctionResult {
        let Some(ledger) = self.ledgers.get_mut(&id) else {
            // The empty-store case. `NotFound` is the host saying "I have no
            // such callable", which the VM turns into a catchable NameError.
            return ExtFunctionResult::NotFound(method.to_owned());
        };
        match method {
            "balance" => ExtFunctionResult::Return(MontyObject::Int(ledger.balance)),
            "charge" => {
                let amount = match args.first() {
                    Some(MontyObject::Int(n)) => *n,
                    _ => {
                        return ExtFunctionResult::Error(MontyException::new(
                            ExcType::TypeError,
                            Some("charge(amount: int)".to_owned()),
                        ));
                    }
                };
                if ledger.balance - amount < -ledger.limit {
                    return ExtFunctionResult::Error(MontyException::new(
                        ExcType::ValueError,
                        Some(format!("charge of {amount} would breach the limit")),
                    ));
                }
                ledger.balance -= amount;
                ExtFunctionResult::Return(MontyObject::Int(ledger.balance))
            }
            _ => ExtFunctionResult::NotFound(method.to_owned()),
        }
    }

    /// Answer a lazy attribute lookup routed to an instance id.
    fn lookup_attr(&self, id: MontyUuid, name: &str) -> NameLookupResult {
        let Some(ledger) = self.ledgers.get(&id) else {
            // Nothing known about this object at all.
            return NameLookupResult::Undefined;
        };
        match name {
            "limit" => NameLookupResult::Value(MontyObject::Int(ledger.limit)),
            _ => NameLookupResult::Undefined,
        }
    }
}

/// What a single driven snippet ended up doing.
struct Driven {
    repl: MontyRepl,
    value: Result<MontyObject, String>,
    /// Suspensions seen, as `kind:name(routed|unrouted)`, in order.
    trace: Vec<String>,
}

/// Feed one snippet and service every suspension it raises against `store`,
/// recording what was routed where. This is the same loop the Durable Object
/// host runs, minus the async.
fn drive(repl: MontyRepl, code: &str, store: &mut HostStore) -> Driven {
    let mut trace = Vec::new();
    let mut progress = match repl.feed_start(code, vec![], quiet()) {
        Ok(p) => p,
        Err(e) => {
            return Driven {
                repl: e.repl,
                value: Err(exc_line(&e.error)),
                trace,
            };
        }
    };

    loop {
        match progress {
            ReplProgress::Complete { repl, value } => {
                return Driven {
                    repl,
                    value: Ok(value),
                    trace,
                };
            }
            ReplProgress::FunctionCall(call) => {
                let routed = call.object_id;
                trace.push(format!(
                    "call:{}({})",
                    call.function_name,
                    routed.map_or_else(|| "plain".to_owned(), |_| "routed to instance".to_owned())
                ));
                let result = match routed {
                    Some(id) => store.call_method(id, &call.function_name, &call.args),
                    // The constructor-style entry point: not routed to an
                    // instance, because there is no instance yet.
                    None if call.function_name == "open_ledger" => {
                        let owner = match call.args.first() {
                            Some(MontyObject::String(s)) => s.to_string(),
                            _ => "unknown".to_owned(),
                        };
                        store.insert(
                            LEDGER_INSTANCE_ID,
                            Ledger {
                                owner,
                                balance: 500,
                                limit: 1_000,
                            },
                        );
                        ExtFunctionResult::Return(store.handle(LEDGER_INSTANCE_ID))
                    }
                    None => ExtFunctionResult::NotFound(call.function_name.clone()),
                };
                progress = match call.resume(result, quiet()) {
                    Ok(p) => p,
                    Err(e) => {
                        return Driven {
                            repl: e.repl,
                            value: Err(exc_line(&e.error)),
                            trace,
                        };
                    }
                };
            }
            ReplProgress::NameLookup(lookup) => {
                let routed = lookup.object_id();
                trace.push(format!(
                    "attr:{}({})",
                    lookup.name,
                    routed.map_or_else(|| "plain".to_owned(), |_| "routed to instance".to_owned())
                ));
                let result = match routed {
                    Some(id) => store.lookup_attr(id, &lookup.name),
                    None => NameLookupResult::Undefined,
                };
                progress = match lookup.resume(result, quiet()) {
                    Ok(p) => p,
                    Err(e) => {
                        return Driven {
                            repl: e.repl,
                            value: Err(exc_line(&e.error)),
                            trace,
                        };
                    }
                };
            }
            other => {
                let repl = other.into_repl();
                return Driven {
                    repl,
                    value: Err("unexpected suspension kind".to_owned()),
                    trace,
                };
            }
        }
    }
}

fn show(label: &str, d: &Driven) {
    let value = match &d.value {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("raised {e}"),
    };
    let trace = if d.trace.is_empty() {
        "no suspension".to_owned()
    } else {
        d.trace.join(", ")
    };
    println!("  {label:<34} {value:<44} [{trace}]");
}

fn write_dump(repl: &MontyRepl, path: &Path) -> usize {
    let bytes = dump(SCRIPT, None, SessionRef::Idle(repl)).expect("dump");
    std::fs::write(path, &bytes).expect("write dump");
    bytes.len()
}

fn read_dump(path: &Path) -> MontyRepl {
    let bytes = std::fs::read(path).expect("read dump");
    let d = Dump::load(&bytes).expect("load dump");
    match d.state {
        Session::Idle(r) => *r,
        _ => panic!("expected an idle session"),
    }
}

/// stage1 — hand the guest a host object, use it, then dump with it still held
/// in a global.
fn stage1(path: &Path) -> ExitCode {
    let mut store = HostStore::default();
    let mut repl = new_repl();

    let d = drive(repl, "ledger = open_ledger('ops')", &mut store);
    show("ledger = open_ledger('ops')", &d);
    repl = d.repl;
    let mut ok = d.value.is_ok();

    for code in [
        "ledger.owner",           // eager attr: crossed with the instance
        "ledger.limit",           // lazy attr: routed to the host
        "ledger.balance()",       // method: routed to the host
        "ledger.charge(120)",     // mutates host-side state
        "ledger.balance()",       // sees the mutation
        "type(ledger).__name__",  // the class is host-defined
    ] {
        let d = drive(repl, code, &mut store);
        show(code, &d);
        ok &= d.value.is_ok();
        repl = d.repl;
    }

    // Keep a plain-Python reference too, so the reload can tell "the instance
    // is gone" apart from "the session is gone".
    let d = drive(repl, "receipts = ['charged 120']", &mut store);
    repl = d.repl;
    ok &= d.value.is_ok();

    let size = write_dump(&repl, path);
    println!("  dumped {size} bytes with the host handle live in `ledger`");
    println!("  host store at exit: {} live ledger(s) — this is what does NOT get written", store.ledgers.len());

    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// stage2 — fresh process, **empty host store**. The handoff's predicted state.
fn stage2(path: &Path) -> ExitCode {
    let mut store = HostStore::default();
    let mut repl = read_dump(path);
    println!("  host store in this process: {} ledgers (nothing was restored)", store.ledgers.len());

    let mut ok = true;
    // Ordinary guest state is untouched.
    let d = drive(repl, "receipts", &mut store);
    show("receipts (plain guest state)", &d);
    ok &= d.value.is_ok();
    repl = d.repl;

    // The eager attribute crossed *into* the dump, so it answers with no host.
    let d = drive(repl, "ledger.owner", &mut store);
    show("ledger.owner (eager attr)", &d);
    ok &= d.value.is_ok();
    repl = d.repl;

    // The handle itself survived: the suspension still arrives, correctly
    // routed. It is only the host side of the mapping that is missing.
    let d = drive(repl, "ledger.balance()", &mut store);
    show("ledger.balance() (no host object)", &d);
    let suspension_still_routed = d.trace.iter().any(|t| t.contains("routed to instance"));
    let raised = d.value.is_err();
    repl = d.repl;

    let d = drive(repl, "ledger.limit", &mut store);
    show("ledger.limit (no host object)", &d);
    let attr_raised = d.value.is_err();
    repl = d.repl;

    // And it is catchable, so a turn can survive it.
    let d = drive(
        repl,
        "try:\n    ledger.balance()\n    caught = 'no error'\nexcept Exception as e:\n    caught = type(e).__name__\ncaught\n",
        &mut store,
    );
    show("the same call, inside try/except", &d);
    let catchable = matches!(&d.value, Ok(MontyObject::String(s)) if s.as_str() != "no error");

    println!();
    println!("  handle survived the dump and still routed by id: {suspension_still_routed}");
    println!("  method call raised rather than silently succeeding:  {raised}");
    println!("  lazy attribute raised too:                          {attr_raised}");
    println!("  the exception is catchable in guest code:            {catchable}");

    if ok && suspension_still_routed && raised && attr_raised && catchable {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// stage3 — fresh process, but the host **rebinds** the same instance id to a
/// newly built object before feeding. The mitigation the handoff rules out.
fn stage3(path: &Path, balance: i64) -> ExitCode {
    let mut store = HostStore::default();
    // A different object entirely: different balance, different limit, built by
    // a process that never saw stage1. Only the id is the same.
    store.insert(
        LEDGER_INSTANCE_ID,
        Ledger {
            owner: "ops".to_owned(),
            balance,
            limit: 250,
        },
    );
    let mut repl = read_dump(path);
    println!("  rebound instance {LEDGER_INSTANCE_ID:?} to a fresh object (balance {balance}, limit 250)");

    let mut ok = true;
    for code in ["ledger.owner", "ledger.limit", "ledger.balance()", "ledger.charge(50)"] {
        let d = drive(repl, code, &mut store);
        show(code, &d);
        ok &= d.value.is_ok();
        repl = d.repl;
    }

    // The new limit is enforced by the new object, proving the host side is
    // genuinely the one answering.
    let d = drive(repl, "ledger.charge(10_000)", &mut store);
    show("ledger.charge(10_000) vs new limit", &d);
    let limit_enforced = d.value.is_err();
    repl = d.repl;

    let d = drive(repl, "receipts", &mut store);
    show("receipts (still the old session)", &d);
    ok &= d.value.is_ok();

    println!();
    println!("  the new object's limit was enforced: {limit_enforced}");
    if ok && limit_enforced { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// stage4 — the id is the contract: rebinding to an id the host does not know
/// fails the same way an empty store does, so a wrong mapping is not silent.
fn stage4(path: &Path) -> ExitCode {
    let mut store = HostStore::default();
    // Right kind of object, wrong id — the mistake a host makes when it
    // regenerates ids instead of persisting them.
    store.insert(
        MontyUuid::from_u128(0xdead_beef),
        Ledger {
            owner: "ops".to_owned(),
            balance: 777,
            limit: 1_000,
        },
    );
    let repl = read_dump(path);
    println!("  store holds a ledger under a *different* id than the guest holds");

    let d = drive(repl, "ledger.balance()", &mut store);
    show("ledger.balance() (id mismatch)", &d);
    let raised = d.value.is_err();

    println!();
    println!("  a mismatched id fails loudly rather than answering wrong: {raised}");
    if raised { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let stage = args.get(1).map(String::as_str).unwrap_or("");
    let path = Path::new(args.get(2).map(String::as_str).unwrap_or("/tmp/monty-i.dump"));
    match stage {
        "stage1" => stage1(path),
        "stage2" => stage2(path),
        "stage3" => {
            let balance = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(999);
            stage3(path, balance)
        }
        "stage4" => stage4(path),
        _ => {
            eprintln!("usage: host-objects-across-dump <stage1|stage2|stage3|stage4> <dump path> [arg]");
            ExitCode::FAILURE
        }
    }
}
