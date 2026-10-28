// A default naming a binding of the same pattern is `any` in the pattern's implied type,
// so only the other elements give the initializer a contextual type.
const { fn1 = (x: number) => 0, fn2 = fn1 } = { fn1: x => x + 1, fn2: x => x + 2 };
const [p = (x: number) => 0, q = p] = [x => x, x => x];
const { g = (s: string) => s.length } = { g: s => s.length };
const { e1, e2 = e1 } = { e1: 1 };
const n: string = e2;
const [a, b = a + 1] = [42];
export {};
