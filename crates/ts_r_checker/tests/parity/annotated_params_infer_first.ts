// A context-sensitive function argument's annotated parameters and return type infer to
// the contextual signature before the function is typed: `(t1: D, t2) => ..` against
// `(t: T, t1: T) => void` gives `T = D`, so `t2` is a `D`.
class C { test: string = ""; }
class D extends C { test2: string = ""; }
declare function test<T extends C>(cb: (t: T, t1: T) => void): void;
declare function test2<T extends C>(cb: (t: T, t1?: T) => T): void;
test((t1: D, t2) => { t2.test2 });
test((t1, t2: D) => { t2.test2 });
test((t1: D, t2) => { t2.missing });
test2((t1, t2?: D) => t1);
test2((t1): D => { t1.test2; return new D(); });
