//! Prints the Experiment A transcript: four probes, in order, with the raw
//! evidence each one observed.

use repl_session_persistence::{q1_session_state, q2_suspend_resume, q3_dynamic_tools, q4_options, Probe};

fn section(title: &str) {
    println!("\n{}", "=".repeat(78));
    println!("{title}");
    println!("{}", "=".repeat(78));
}

fn main() {
    section("Q1 — what survives between feed_run calls?");
    let mut p = Probe::new();
    let q1 = q1_session_state::run(&mut p);
    for l in p.lines() {
        println!("{l}");
    }
    println!("\nFACTS: {q1:#?}");

    section("Q2 — suspend/resume through a host function");
    let mut p = Probe::new();
    let q2 = q2_suspend_resume::run(&mut p);
    for l in p.lines() {
        println!("{l}");
    }
    println!("\nFACTS: {q2:#?}");

    section("Q3 — can the tool set vary per turn? (monty #190)");
    let mut p = Probe::new();
    let q3 = q3_dynamic_tools::run(&mut p);
    for l in p.lines() {
        println!("{l}");
    }
    println!("\nFACTS: {q3:#?}");
    println!("\ninputs_cannot_carry_a_tool: {}", q3_dynamic_tools::inputs_cannot_carry_a_tool());

    section("Q4 — CompileOptions / ResourceTracker / type_check (monty #872)");
    let mut p = Probe::new();
    let q4 = q4_options::run(&mut p);
    for l in p.lines() {
        println!("{l}");
    }
    println!("\nFACTS: {q4:#?}");
    println!("\ntype_check_availability: {}", q4_options::type_check_availability());
}
