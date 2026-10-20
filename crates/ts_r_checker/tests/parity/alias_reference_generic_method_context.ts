// A generic method read through an alias of a reference (`type AnyBuilder = Builder<any>`,
// whose `this` is the alias) and through the plain reference has the same type parameters:
// a method contextually typed by one relates to the other.
type Parser = { parse(x: unknown): unknown };
interface Builder<TIn> {
  input<$P extends Parser>(schema: TIn extends string ? $P : $P[]): Builder<$P>;
  self(): this;
}
type AnyBuilder = Builder<any>;
export const b: AnyBuilder = {
  input(input) {
    return b;
  },
  self() {
    return b;
  },
};
const n: number = b.self();
