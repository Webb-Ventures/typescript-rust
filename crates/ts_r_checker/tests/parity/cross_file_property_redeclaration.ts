// @filename: a.d.ts
interface Foo { a: number; }
interface Array<T> { length: string; }
// @filename: b.d.ts
interface Foo { a: string; }
interface Foo { a: boolean; }
