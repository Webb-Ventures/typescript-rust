// Anything is assignable to `{} | null | undefined`, `unknown` included; `{} | null` alone
// still rejects `unknown`.
function f(x: unknown, v: void) {
  let a: {} | null | undefined = x;
  let b: {} | null | undefined = v;
  let c: {} | null = x;
}
