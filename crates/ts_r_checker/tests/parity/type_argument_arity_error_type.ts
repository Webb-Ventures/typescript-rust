// A reference with the wrong number of type arguments is the error type; a type merged
// with a generic function has only the type declaration's type parameters.
class A<T> { a?: T }
export var x: () => A;
export const f = (): A => null;
type Constructor<T> = new (...args: any[]) => T;
interface Tagged { _tag: string }
export function Tagged<T extends Constructor<{}>>(superClass: T): Constructor<Tagged> & T {
  return null!;
}
interface Tag<in out Id, in out Value> { readonly _op: "Tag"; id: Id; v: Value }
declare function Tag<const Id extends string>(id: Id): Tag<Id, number>;
export type U<Self> = Tag<Self, number>;
export const t: Tag<"a", number> = Tag("a");
