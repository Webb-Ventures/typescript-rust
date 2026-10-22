// A covariant inference gives way to the contravariant one when one of its own candidates
// (`Decorator` and `Modifier` from the union of arrays), or a candidate of a type parameter
// constrained to it (`U`'s `C`), is not assignable to it (tsc's coAndContraVariantInferences
// 2 and 4): `T` is `Node` and `A` here, so none of these calls is an error.
const enum Kind {
  Modifier,
  Decorator,
}
interface Node {
  kind: Kind;
}
interface Modifier extends Node {
  kind: Kind.Modifier;
}
interface Decorator extends Node {
  kind: Kind.Decorator;
}
declare function isModifier(node: Node): node is Modifier;
declare function isDecorator(node: Node): node is Decorator;
declare function every<T, U extends T>(
  array: readonly T[],
  callback: (element: T) => element is U,
): array is readonly U[];
declare const modifiers: readonly Decorator[] | readonly Modifier[];
every(modifiers, isModifier);
every(modifiers, isDecorator);

interface A {
  a: string;
}
interface B extends A {
  b: string;
}
interface C extends A {
  c: string;
}
declare function cast<T, U extends T>(x: T, test: (x: T) => x is U): U;
declare function isC(x: A): x is C;
declare const b: B;
const c: C = cast(b, isC);
