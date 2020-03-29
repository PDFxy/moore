//! A SystemVerilog parser generator.

#[macro_use]
extern crate log;

pub mod ast;
pub mod codegen;
pub mod context;
pub mod factor;
pub mod ll;
pub mod lr;
pub mod opt;
pub mod parser;
pub mod populate;

use crate::context::{Context, ContextArena};
use anyhow::{anyhow, Result};
use clap::{App, Arg};
use std::{fs::File, io::Write};

fn main() -> Result<()> {
    let matches = App::new("svlog-pargen")
        .version(clap::crate_version!())
        .author(clap::crate_authors!())
        .about("A parser generator for SystemVerilog.")
        .arg(
            Arg::with_name("grammar")
                .takes_value(true)
                .required(true)
                .number_of_values(1),
        )
        .arg(
            Arg::with_name("dump-init")
                .short("d")
                .long("dump-init")
                .help("Dump the grammar after basic factorization.")
                .takes_value(true)
                .number_of_values(1),
        )
        .arg(
            Arg::with_name("dump-final")
                .short("f")
                .long("dump-final")
                .help("Dump the grammar after full processing.")
                .takes_value(true)
                .number_of_values(1),
        )
        .arg(
            Arg::with_name("dump-states")
                .short("s")
                .long("dump-states")
                .help("Dump the LR(1) parser states.")
                .takes_value(true)
                .number_of_values(1),
        )
        .arg(
            Arg::with_name("dump-conflicts")
                .short("c")
                .long("dump-conflicts")
                .help("Dump the conflicts.")
                .takes_value(true)
                .number_of_values(1),
        )
        .arg(
            Arg::with_name("output")
                .short("o")
                .long("output")
                .help("Emit code into this file.")
                .takes_value(true)
                .number_of_values(1),
        )
        .get_matches();

    env_logger::Builder::from_default_env()
        .format_timestamp(None)
        .init();

    let grammar = parse_grammar(&std::fs::read_to_string(
        matches.value_of("grammar").unwrap(),
    )?)?;

    // Parse the grammar and populate the context.
    let arena = ContextArena::default();
    let mut context = Context::new(&arena);
    populate::add_ast(&mut context, grammar);
    info!(
        "Grammar has {} productions, {} nonterminals, {} terminals",
        context.prods.values().flatten().count(),
        context.nonterms().count(),
        context.terms().count(),
    );

    // Perform initial minimization of the grammar to remove redundancies.
    context.minimize();

    // Dump this initial grammar if requested.
    if let Some(path) = matches.value_of("dump-init") {
        info!("Dumping grammar to `{}`", path);
        let mut f = File::create(path)?;
        for ps in context.prods.values() {
            for p in ps {
                write!(f, "{}\n", p)?;
            }
        }
    }

    // Create the LR(1) table.
    lr::build_lr(&mut context);

    // Dump the LR(1) states if requested.
    if let Some(path) = matches.value_of("dump-states") {
        info!("Dumping states to `{}`", path);
        let mut f = File::create(path)?;
        lr::dump_states(&context.lr_table, &mut f)?;
    }

    // Dump the conflicts if requested.
    if let Some(path) = matches.value_of("dump-conflicts") {
        info!("Dumping conflicts to `{}`", path);
        let mut f = File::create(path)?;
        let num_conflicts = lr::dump_conflicts(&context.lr_table, &mut f)?;
        info!("Found {} conflicts", num_conflicts);
    }

    if false {
        // Perform basic LL(1) transformations.
        for i in 1.. {
            info!("Simplifying grammar (step {})", i);
            let mut modified = false;
            modified |= factor::remove_epsilon_derivation(&mut context);
            modified |= factor::remove_indirect_left_recursion(&mut context);
            modified |= factor::remove_direct_left_recursion(&mut context);
            context.minimize();
            if !modified {
                break;
            }
        }
        factor::left_factorize_simple(&mut context);
        context.minimize();
        info!(
            "Grammar has {} productions, {} nonterminals, {} terminals",
            context.prods.values().flatten().count(),
            context.nonterms().count(),
            context.terms().count(),
        );

        // Optimize the grammar.
        for i in 1..50 {
            info!("Optimizing grammar (step {})", i);
            let mut modified = false;
            modified |= opt::optimize(&mut context);
            context.minimize();
            if !modified {
                break;
            }
            // std::io::stdin().read_line(&mut Default::default()).unwrap();
        }

        // ll::build_ll(&mut context);
        // ll::dump_ambiguities(&context);

        // debug!("LL(1) Table:");
        // for (nt, ts) in &context.ll_table {
        //     for (t, ps) in ts {
        //         for p in ps {
        //             debug!("  [{}, {}] = {}", nt, t, p);
        //         }
        //     }
        // }
    }

    // Dump this final grammar if requested.
    if let Some(path) = matches.value_of("dump-final") {
        info!("Dumping grammar to `{}`", path);
        let mut f = File::create(path)?;
        for ps in context.prods.values() {
            for p in ps {
                write!(f, "{}\n", p)?;
            }
        }
    }

    // Generate code.
    if let Some(path) = matches.value_of("output") {
        info!("Generating code in `{}`", path);
        {
            let mut f = File::create(path)?;
            codegen::codegen(&mut context, &mut f)?;
        }
        std::process::Command::new("rustfmt").arg(path).output()?;
    }

    Ok(())
}

/// Parse a grammar string.
pub fn parse_grammar(input: impl AsRef<str>) -> Result<ast::Grammar> {
    parser::GrammarParser::new()
        .parse(input.as_ref())
        .map_err(|e| anyhow!("Grammar syntax error: {}", e))
}