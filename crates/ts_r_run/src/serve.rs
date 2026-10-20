//! `ts-r-run --serve`: the long-lived compiler behind strategy "hook". Reads requests from
//! stdin, one per line, and answers each in order on stdout (protocol in
//! `js/hooks-async.mjs`). Exits when stdin closes, i.e. when Node exits.

use std::io::BufRead as _;

use crate::compile::Format;
use crate::graph::Engine;

fn reply(out: &mut impl std::io::Write, ok: bool, format: &str, body: &str) -> std::io::Result<()> {
    writeln!(
        out,
        "{}\t{format}\t{}",
        if ok { "ok" } else { "err" },
        body.len()
    )?;
    out.write_all(body.as_bytes())?;
    out.flush()
}

pub fn serve(engine: &Engine) -> std::io::Result<()> {
    let stdin = std::io::stdin().lock();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lines() {
        let line = line?;
        let mut parts = line.split('\t');
        match parts.next() {
            Some("C") => {
                let path = crate::project::absolute(parts.next().unwrap_or(""));
                match engine.compile_file(&path) {
                    Ok(u) => {
                        let f = match u.compiled.format {
                            Format::Esm => "esm",
                            Format::Cjs => "cjs",
                        };
                        reply(&mut out, true, f, &u.compiled.code)?;
                    }
                    Err(e) => reply(&mut out, false, "-", &e)?,
                }
            }
            Some("R") => {
                let require = parts.next() == Some("1");
                let parent = crate::project::absolute(parts.next().unwrap_or(""));
                let spec = parts.next().unwrap_or("");
                let r = engine.project.resolve(spec, &parent, require);
                reply(&mut out, true, "-", r.as_deref().unwrap_or(""))?;
            }
            _ => reply(&mut out, false, "-", "bad request")?,
        }
    }
    engine.flush();
    Ok(())
}
