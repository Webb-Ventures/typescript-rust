// Type arguments that satisfy their constraints only through generic reasoning
// (declaration files of type-fest, @babel/traverse, execa, react-hook-form, express).

// The true branch of `Extract<K, keyof O>` is `K & keyof O`.
type Distributed<O, K extends keyof O> = Pick<O, K>;
type DistributedPick<O, KeyType extends PropertyKey> = Distributed<O, Extract<KeyType, keyof O>>;

// A parameter constrained to a mapped type indexed by its own keys.
interface Node { type: string; kids: Node[]; tag: string | null; list: (Node | null)[] }
type KeysOfArrays<N extends Node> = {
    [P in string & keyof N]-?: N[P] extends (Node | null)[] ? P : never;
}[string & keyof N];
type ListType<N, K extends keyof N> = N[K] extends any[] ? N[K][number] : never;
declare function push<N extends Node, K extends KeysOfArrays<N>, X extends ListType<N, K>>(n: N, k: K, x: X): void;

// The true branch of a conditional's constraint narrows the check type.
type Paths<T extends Node | readonly Node[]> = T extends readonly Node[] ? T : never;
type LT<N, K extends keyof N> = N[K] extends Array<infer P> ? (P extends Node ? P : never) : never;
type Ins<T extends Node> = T | readonly T[] | [T, ...T[]];
declare function pushAll<T extends Node, K extends keyof T, Nodes extends Ins<LT<T, K>>>(k: K, n: Nodes): Paths<Nodes>;

// An alias referenced before it is resolved (`[Traversable]`) is the aliased type.
type Traversable = object;
type NK<T extends Traversable> = keyof T;
type IsNever<T> = [T] extends [never] ? true : false;
type CI<T> = IsNever<Extract<T, ReadonlyArray<any>>> extends true ? false : true;
type KeysImpl<T> = [T] extends [Traversable] ? CI<T> extends true ? NK<T> : NK<T> : never;
export type Keys<T> = KeysImpl<NonNullable<T>>;

// A generic intersection against a union relates through its constraint.
type OA = { enc?: 'a'; x?: string | ((c: string) => void) };
type OB = { enc: 'b'; x?: number | ((c: number) => void) };
type Options = OA | OB;
type M<O extends Options = Options> = { o: O; bind: <N extends Options = {}>(n: N) => M<O & N> };

// Still reported.
type Bad<O, K extends keyof O> = Distributed<O, K | 'zz'>;
export {};
