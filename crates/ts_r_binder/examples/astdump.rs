fn main() {
    let path = std::env::args().nth(1).unwrap();
    let src = std::fs::read_to_string(&path).unwrap();
    let r = ts_r_parser::parse_path(&path, &src);
    print!("{}", r.ast.dump());
    for d in r.diagnostics {
        println!("diag {} {:?} {}", d.code, d.span, d.message);
    }
}
