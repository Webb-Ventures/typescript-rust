// An ambient module name is a pattern with at most one `*` (tsc 7.0.2:
// ambientDeclarationsPatterns_tooManyAsterisks): TS5061 at the name.
declare module "too*many*asterisks" { }
declare module "one*star" { }
declare module "plain" { }
