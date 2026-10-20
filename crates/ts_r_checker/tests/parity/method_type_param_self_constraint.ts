// expect-type (`expectTypeOf(x).toMatchTypeOf<Meta | undefined>()`, whose `Expected` is
// constrained by `Extends<Actual, Expected> extends true ? unknown : MismatchInfo<...>`): the
// type parameters of a method of an instantiated interface get copies whose constraints
// refer to the copies, so explicit type arguments are checked against the constraint
// instantiated with them.
type Ext<L, R> = [L] extends [R] ? true : false;
interface Box<A> {
  m<E extends Ext<A, E> extends true ? unknown : { mismatch: A }>(): void;
  n<E extends Ext<E, A> extends true ? unknown : { mismatch: A }>(): void;
}
declare const b: Box<string | undefined>;
b.m<string | undefined>();
b.m<string>();
b.m<number>();
b.n<string>();
b.n<number>();
declare function g<E extends Ext<string, E> extends true ? unknown : { mismatch: 1 }>(): void;
g<string>();
g<number>();
export {};
