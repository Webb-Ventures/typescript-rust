type O = { a: 1 };
type A<P extends string> = P extends `${infer Q}/x` ? { [K in Q | keyof O]: 1 } : never;
type ExtractParams<Path extends string> = string extends Path
    ? Record<string, string>
    : Path extends `${infer _Start}:${infer Param}/${infer Rest}`
      ? { [K in Param | keyof ExtractParams<`/${Rest}`>]: string }
      : Path extends `${infer _Start}:${infer Param}`
        ? { [K in Param]: string }
        : never;
type L<P extends string> = P extends `${infer Q}/` ? Q["length"] : never;
type N<P extends string> = P extends `${infer Q}/` ? Q["nope"] : never;
type Box<T extends number> = T;
type B<P extends string> = P extends `${infer Q}/` ? Box<Q> : never;
export type X = ExtractParams<"/a/:id/b/:name">;
export const x: X = { id: "", name: "" };
export const y: X = { id: "" };
export const l: L<"abc/"> = 3;
export type { A, N, B };
