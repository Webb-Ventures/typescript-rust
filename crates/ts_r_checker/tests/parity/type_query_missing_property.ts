// `typeof a.p` reports a missing property like the access `a.p` (TS2339, TS7017 through
// globalThis), reads index signatures, and narrows a generic root through its constraint.
declare const r: Record<string, number>;
type A = typeof r.foo;
declare const u: { a: 1 } | { b: 2 };
type B = typeof u.a;
function f<T extends { x: number }>(t: T) { type C = typeof t.x; type D = typeof t.y; }
enum E { A }
type F = typeof E.B;
namespace N { export const v = 1; }
type G = typeof N.w;
class K { static s = 1; m() { type H = typeof this.zz; } }
type I = typeof K.t;
declare const arr: number[];
type J = typeof arr.length;
type L = typeof arr[0];
declare const o: any;
type M = typeof o.a.b;
declare const s: string;
type P = typeof s.nope;
type Fish = { type: 'fish', hasFins: true }
type Dog = { type: 'dog', saysWoof: true }
type Pet = Fish | Dog;
function handleDogBroken<PetType extends Pet>(pet: PetType) {
    if(pet.type === 'dog') {
        const _okay2: typeof pet.saysWoof = pet.saysWoof;
    }
    type Q = typeof pet.hasFins;
}
function g<T extends { a: 1 } | { a: 2 }>(t: T) { const x: typeof t.a = t.a; type Z = typeof t; }
let zz = { b: 1 };
type T0 = typeof zz.c;
let gx: typeof globalThis.no3 = 1;
const arrow = () => {
    let y: typeof this.no = 1;
};
