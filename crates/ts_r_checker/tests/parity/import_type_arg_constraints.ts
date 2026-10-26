// The type arguments of `import("m").Foo<T>` are checked against Foo's type parameters.
// @filename: file1.ts
export type Foo<T extends string> = { foo: T };
export interface Bar<A, B = number> { a: A; b: B }
// @filename: file2.ts
type X<T> = import("./file1").Foo<T>;
type Y = import("./file1").Foo<"ok">;
type Z = import("./file1").Bar<1, 2, 3>;
export type { X, Y, Z };
