// An assignment or comma expression elaborates its right operand: the error is at the
// property deep inside, not at the declaration.
interface Foo { a: { b: { c: { d: string } } } }
let q: Foo["a"] | undefined;
const x1: Foo = { a: q = { b: { c: { d: 42 } } } };
const x2: Foo = (void 0, { a: { b: { c: { d: 42 } } } });
const x3: Foo = { a: { b: ({ c: { d: 42 } }) } };
const x4: Foo = { a: q = { b: ({ c: { d: 42 } }) } };
export {}
