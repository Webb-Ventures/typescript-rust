# Security policy

## Reporting a vulnerability

Please report security issues privately, not in a public issue or pull request. Use
[GitHub's private vulnerability reporting](https://github.com/Webb-Ventures/typescript-rust/security/advisories/new):
**Security**, then **Report a vulnerability**.

Include what is affected (which tool and version), how to reproduce it, and what an attacker could
do with it. We acknowledge reports within three working days and keep you updated until a fix is
released. Reporters are credited in the advisory unless they prefer not to be.

## What counts

ts-r reads untrusted input all the time: source files, `tsconfig.json`, `package.json`, lockfiles
and `node_modules`. Within scope:

- a crafted input that makes a tool run code, write outside its output directories, or read files
  it shouldn't;
- the dev server (`ts-r-dev`) or language server (`ts-r-lsp`) serving or exposing files beyond
  what they are configured to;
- the npm packages fetching or running anything other than their own platform binary;
- crashes, hangs or unbounded memory use on crafted input (also fine as a public issue if they
  can't be turned into anything worse).

## Supported versions

Fixes go into the latest release. ts-r is pre-1.0, so older releases are not patched.
