// A mapped type that removes `?` is unmeasurable: `Required<X>` arguments relate only when
// identical, else by structure (and so is a type that holds one). Indexed access and
// mapped templates measure reliably: `Pick<X, "x">` through an alias is covariant in `X`.
declare let a: Required<{ a?: 1; x: 1 }>;
declare let b: Required<{ b?: 1; x: 1 }>;
a = b;
b = a;
interface Holder<T> {
    a: Required<T>;
}
declare let ha: Holder<{ a?: 1; x: 1 }>;
declare let hb: Holder<{ b?: 1; x: 1 }>;
ha = hb;
class A { x = "A"; y = 0; }
class B { x = "B"; z = true; }
type Picked<X extends { x: unknown }> = Pick<X, "x">;
declare let pa: Picked<A>;
declare let pb: Picked<B>;
pb = pa;
export {};
