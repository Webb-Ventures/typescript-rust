// Assertion-wrapped operands are references; non-null keeps the constant check.
let a = 10;
(a as number)++;
(a satisfies number)++;
a!++;
(a!)++;
(a as any) += 1;
const k = 1;
(k as number)++;
(k satisfies number)++;
k!++;
(k!)++;
k! = 2;

// `typeof x === "Object"` keeps object members when true and drops them when false.
class C { c = 1 }
declare const x: string | C | (() => void) | undefined | { a: 1 };
if (typeof x === "Object") {
    const n: number = x;
} else {
    const n: number = x;
}
declare const u: unknown;
if (typeof u === "Object") { const n: number = u; } else { const n: number = u; }
