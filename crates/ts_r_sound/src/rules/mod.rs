//! The rules. Each module reports through `Ctx::report`, which drops rules the file's level
//! or `[sound.rules]` switches off; precedence and hatches are applied by the driver.

mod claims;
mod exact_optional;
mod exceptions;
mod guards;
mod init;
mod invalidation;
mod overloads;
mod promises;
mod relations;
mod syntax;

use crate::ctx::Ctx;

pub fn run_all(cx: &mut Ctx<'_, '_, '_>) {
    syntax::run(cx);
    overloads::run(cx);
    relations::run(cx);
    claims::run(cx);
    exact_optional::run(cx);
    guards::run(cx);
    init::run(cx);
    promises::run(cx);
    invalidation::run(cx);
    exceptions::run(cx);
}
