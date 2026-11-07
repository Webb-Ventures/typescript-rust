// Variance annotations: a written `in`/`out` replaces measurement, is checked against the
// structure (TS2636, the markers print `sub-T` / `super-T`) and is only allowed on aliases
// of object, function, constructor and mapped types (TS2637).
type Getter<out T> = { get(): T };
type Setter<in T> = { set: (v: T) => void };
type Both<in out T> = { v: T };
type Wrong<in T> = { get(): T };
type NotObject<out T> = T[];
interface Merged<out T> {}
interface Merged<in T> {}
class Box<out T> { constructor(readonly v: T) {} }
declare let g1: Getter<string>, g2: Getter<unknown>;
g2 = g1;
g1 = g2;
declare let s1: Setter<string>, s2: Setter<unknown>;
s1 = s2;
s2 = s1;
declare let b1: Both<string>, b2: Both<unknown>;
b2 = b1;
declare let m1: Merged<string>, m2: Merged<unknown>;
m2 = m1;
declare let x1: Box<string>, x2: Box<unknown>;
x2 = x1;
class C { in a = 0; }
export type { Wrong, NotObject };
