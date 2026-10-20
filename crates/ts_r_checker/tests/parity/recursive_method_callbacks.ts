// Method callback parameters are compared one callback level deep (tsc's callback check);
// deeper ones go through the guarded relation, so self-referencing callbacks terminate.
interface A { f(cb: A['f']): void }
interface B { f(cb: B['f']): void }
declare let a: A;
declare let b: B;
a = b;
interface C { f(cb: (g: (x: string) => void) => void): void }
interface D { f(cb: (g: (x: number) => void) => void): void }
declare let c: C;
declare let d: D;
c = d;
