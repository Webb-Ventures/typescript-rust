// Inside `T extends X ? <here> : ...` a type argument mentioning `T` sees `X & T` when its
// constraint is checked (no TS2344); the false branch and other positions do not.
type P<T> = T extends abstract new (args: any) => any ? InstanceType<T> : never;
type Q<T> = T extends (...args: any) => any ? ReturnType<T> : T extends string ? Uppercase<T> : never;
type R<T, U> = T extends string ? U extends number ? [Uppercase<T>, U] : never : never;
type K<T> = T extends readonly unknown[] ? T[number] extends string ? Uppercase<T[number]> : 0 : 1;
interface FN<FC extends undefined | void> { c: FC }
class C<FC = unknown> {
  fetch(o: unknown extends FC ? 1 : FC extends undefined | void ? FN<FC> : 2): void {}
}
type Bad2<T> = T extends string ? never : Uppercase<T>;
type Bad3<T> = Uppercase<T>;
export type { P, Q, R, K, C, Bad2, Bad3 };
