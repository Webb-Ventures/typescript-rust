// Keys an indexed access type cannot read: TS2339, TS2537, TS2538.
interface Shape {
  name: string;
}
type Shape2<T> = T extends { a: infer A } ? A : never;
type S = Shape2<{ a: { data: { x: number } } }>;
type A1 = S['data']['kind'];
type A2 = Shape['name' | 'foo'];
type A3 = Shape[string];
type A4 = Shape[string | number];
type A5 = Shape[boolean];
type A6 = Shape[string | boolean];
type A7 = Shape[any];
type A8 = string[]['0'];
type A9 = string[]['0.0'];
type A10 = string[][string];
interface Dict {
  [k: string]: number;
}
type A11 = Dict[any];
type A12 = Shape extends Record<'foo', string> ? Shape['foo'] : never;
declare const w: (typeof globalThis)['globalThis'];
declare const w2: (typeof globalThis)['nope'];
