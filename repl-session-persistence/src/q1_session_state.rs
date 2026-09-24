//! Q1 — what survives between `feed_run` calls on one `MontyRepl`?
//!
//! Plain globals, mutated containers, user functions, imported modules, and
//! (the handoff doubted this) `class` statements.

use monty_types::MontyObject;

use crate::{exc_summary, new_repl, quiet, say, Probe};

#[derive(Debug)]
pub struct Facts {
    pub plain_global: MontyObject,
    pub mutated_list: MontyObject,
    pub user_function: MontyObject,
    pub redefined_function: MontyObject,
    pub imported_module: MontyObject,
    pub from_import: MontyObject,
    /// `class` defined in one feed, instantiated + called in later feeds.
    pub class_instance_method: MontyObject,
    pub class_is_isinstance: MontyObject,
    /// Does the class name show up in `function_names()` / `has_function()`?
    pub class_in_function_names: bool,
    pub function_names: Vec<String>,
    /// A snippet that binds a name and *then* raises: does the binding stick?
    pub binding_before_raise: MontyObject,
    pub raise_message: String,
    /// A snippet with a *syntax* error — does it disturb the session?
    pub syntax_error_message: String,
    pub state_after_syntax_error: MontyObject,
}

pub fn run(p: &mut Probe) -> Facts {
    let mut repl = new_repl();
    let mut feed = |repl: &mut monty::MontyRepl, p: &mut Probe, code: &str| {
        let out = repl.feed_run(code, vec![], quiet());
        say!(p, "  feed {code:?}\n    -> {out:?}");
        out
    };

    say!(p, "-- plain globals survive, and are not replayed --");
    feed(&mut repl, p, "counter = 41").unwrap();
    feed(&mut repl, p, "counter = counter + 1").unwrap();
    let plain_global = feed(&mut repl, p, "counter").unwrap();

    say!(p, "-- container mutation in a later feed is visible --");
    feed(&mut repl, p, "items = [1, 2]").unwrap();
    feed(&mut repl, p, "items.append(3)").unwrap();
    feed(&mut repl, p, "d = {'a': 1}\nd['b'] = 2").unwrap();
    let mutated_list = feed(&mut repl, p, "(items, d)").unwrap();

    say!(p, "-- user-defined functions survive, and rebind on redefinition --");
    feed(&mut repl, p, "def double(n):\n    return n * 2\n").unwrap();
    let user_function = feed(&mut repl, p, "double(21)").unwrap();
    feed(&mut repl, p, "def double(n):\n    return n * 3\n").unwrap();
    let redefined_function = feed(&mut repl, p, "double(21)").unwrap();

    say!(p, "-- imports survive --");
    feed(&mut repl, p, "import math").unwrap();
    let imported_module = feed(&mut repl, p, "math.sqrt(16.0)").unwrap();
    feed(&mut repl, p, "from dataclasses import dataclass").unwrap();
    let from_import = feed(
        &mut repl,
        p,
        "@dataclass\nclass Pt:\n    x: int\n    y: int\n",
    )
    .and_then(|_| repl.feed_run("Pt(1, 2).x", vec![], quiet()));
    say!(p, "    (dataclass round-trip) -> {from_import:?}");
    let from_import = from_import.unwrap_or(MontyObject::Repr("<failed>".into()));

    say!(
        p,
        "-- classes: the handoff claims they are 'not yet definable inside Monty' --"
    );
    feed(
        &mut repl,
        p,
        "class Point:\n    kind = 'cartesian'\n\n    def __init__(self, x):\n        self.x = x\n\n    def doubled(self):\n        return self.x * 2\n\n    def __repr__(self):\n        return f'Point({self.x})'\n",
    )
    .unwrap();
    // separate feeds: the class object itself has to survive the feed boundary
    feed(&mut repl, p, "pt = Point(21)").unwrap();
    let class_instance_method = feed(&mut repl, p, "pt.doubled()").unwrap();
    let class_is_isinstance = feed(
        &mut repl,
        p,
        "(isinstance(pt, Point), type(pt).__name__, Point.kind, repr(pt))",
    )
    .unwrap();

    let function_names: Vec<String> = repl.function_names().iter().map(|s| (*s).to_owned()).collect();
    let class_in_function_names = repl.has_function("Point");
    say!(p, "  repl.function_names() -> {function_names:?}");
    say!(p, "  repl.has_function(\"Point\") -> {class_in_function_names}");

    say!(p, "-- a snippet that binds then raises: bindings stick (CPython REPL semantics) --");
    let err = feed(&mut repl, p, "bound_before_raise = 7\nraise ValueError('boom')")
        .expect_err("expected the raise to surface");
    let raise_message = exc_summary(&err);
    let binding_before_raise = feed(&mut repl, p, "bound_before_raise").unwrap();

    say!(p, "-- a snippet that fails to *compile* leaves the session untouched --");
    let err = feed(&mut repl, p, "def broken(:").expect_err("expected a syntax error");
    let syntax_error_message = exc_summary(&err);
    let state_after_syntax_error = feed(&mut repl, p, "(counter, double(1), pt.doubled())").unwrap();

    Facts {
        plain_global,
        mutated_list,
        user_function,
        redefined_function,
        imported_module,
        from_import,
        class_instance_method,
        class_is_isinstance,
        class_in_function_names,
        function_names,
        binding_before_raise,
        raise_message,
        syntax_error_message,
        state_after_syntax_error,
    }
}
