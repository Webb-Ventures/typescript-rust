//! `ts-r`: the launcher binary. All logic lives in the library so it can be tested.

fn main() {
    let code = ts_r_cli::run(std::env::args_os().skip(1).collect());
    std::process::exit(code);
}
