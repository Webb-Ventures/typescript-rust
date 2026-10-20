//! `ts-r-run [options] <file> [args...]`; see [`ts_r_run`].

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(ts_r_run::main_with(&args));
}
