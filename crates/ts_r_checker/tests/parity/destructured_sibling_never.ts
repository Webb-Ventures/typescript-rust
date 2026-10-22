type X = { kind: "a", a: [1] } | { kind: "b", a: [] }

function foo(x: X): 1 {
  const { kind, a } = x;
  switch (kind) {
    case "a":
      return a[0];
    case "b":
      return 1;
    default:
      const [n] = a;
      return a;
  }
}
function bar({ kind, a }: X) {
  if (kind === "a") return;
  if (kind === "b") return;
  const z: string = a;
}
export {};
