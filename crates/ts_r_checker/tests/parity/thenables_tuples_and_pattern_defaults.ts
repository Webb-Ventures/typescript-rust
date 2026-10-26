declare class Thenable { then(): void; }
declare let thenable: Thenable;
async function fn6(): Thenable { }
async function fn13() { return thenable; }
async function fn19() { await thenable; }

declare const b4: [number?];
b4.length = 0;
declare const b1: readonly [number?];
b1.length = 0;

{
    let a: 0 | 1 = 1;
    let b: 0 | 1 | 9;
    [{ [a]: b } = [9, a = 0] as const] = [];
    const bb: 9 = b;
    const aa: 0 = a;
}

function f11<T extends unknown[]>(t: T, m: [...T], r: readonly [...T]) {
    t = m;
    t = r;
    m = t;
    r = t;
}

function f15<T extends string[]>(k1: keyof [...T], k3: keyof [1, 2, ...T]) {
    k1 = 'length';
    k1 = 'slice';
    k3 = '1';
    k3 = '2';
}
export { fn6, fn13, fn19, f11, f15 };
