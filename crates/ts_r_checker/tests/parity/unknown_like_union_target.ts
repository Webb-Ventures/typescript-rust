type A = {};
type B = {};
type AB = A | B;
function f(u: unknown, v: string | undefined) {
    let x1: {} = u;
    let x2: {} | null | undefined = u;
    let x3: AB | null | undefined = u;
    let x4: {} | null = u;
    let x5: {} | undefined = u;
    let x6: { a: string } | null | undefined = u;
    let x7: {} | null | undefined = v;
}
