// tsc 7.0.2 reports TS2322 here with the target type printed as `keyof Foo`; ts-r-check 0.0.0
// prints it expanded, `"a" | "b"` (a known checker gap, ts_r_checker AGENTS.md, "Known gaps").
// If the checker catches up, pick another known difference: the drop-in test checks that this
// fixture still fails the parity check.
interface Foo {
  a: 1;
  b: 2;
}
export const k: keyof Foo = "c";
