//! cella-universe: machines as artifacts -- branch, archive,
//! inspect. Running is the only state a universe verb refuses.

mod universe;

const VERBS: &[&str] = &["branch", "archive", "inspect"];

fn fatal(msg: &str) -> ! {
    eprintln!("cella: fatal: {msg}");
    std::process::exit(1);
}

fn usage_error(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = argv.first().cloned() else {
        usage_error("usage: cella-universe <branch|archive|inspect> ...")
    };
    if !VERBS.contains(&verb.as_str()) {
        usage_error(&format!(
            "cella-universe does not own the verb {verb:?} -- its verbs: {}",
            VERBS.join(", ")
        ));
    }
    let args = &argv[1..];
    // The witnessed border (1.6.11): the source machine's book.
    if let Err(e) = cella_libs::audit::witness(args.first().map(|s| s.as_str()), &verb, args) {
        fatal(&e);
    }
    let ok = match verb.as_str() {
        "branch" => match args {
            [src, dst] => universe::branch(src, dst),
            _ => Err("usage: cella branch <existing-vm> <new-vm>".to_string()),
        },
        "archive" => match args {
            [vm] => universe::archive(vm),
            _ => Err("usage: cella archive <vm>".to_string()),
        },
        "inspect" => match args {
            [vm] => universe::inspect(vm),
            _ => Err("usage: cella inspect <vm>".to_string()),
        },
        _ => unreachable!(),
    };
    match ok {
        Ok(()) => std::process::exit(0),
        Err(e) => fatal(&e),
    }
}
