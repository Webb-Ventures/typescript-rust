// `typeof x.p` in a type is narrowed where it stands, like `typeof x`.
interface I<T> {
  p: T;
  q: { r: T };
}
function e(x: I<"A" | "B">) {
  if (x.p === "A") {
    let a: "A" = null as unknown as typeof x.p;
    let b: "A" = null as unknown as typeof x.q.r;
  }
  if (x.q.r === "B") {
    let c: "B" = null as unknown as typeof x.q.r;
  }
}
