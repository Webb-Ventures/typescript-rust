// Type arguments that are generic satisfy constraints through their own constraints
// (from execa, @vitest/spy, type-fest, zod, @reduxjs/toolkit and @types/node).
type HasIpcOption<I extends boolean | undefined> = I extends true ? 1 : 0;
type Opts = { ipc?: boolean; x: 1 } | { ipc?: boolean; x: 2 };
type HasIpc<O extends Opts> = HasIpcOption<O['ipc']>;
type S<I extends string> = I;
type E = { s?: string };
type Narrowed<O extends E> = O['s'] extends string ? S<O['s']> : never;
type Unnarrowed<O extends E> = S<O['s']>;

type Procedure = (...args: any[]) => any;
type Constructable = new (...args: any[]) => any;
type Mock<T extends Procedure | Constructable> = T extends Constructable
    ? T extends Procedure ? { new (...args: ConstructorParameters<T>): InstanceType<T>; (...args: Parameters<T>): ReturnType<T> } : never
    : never;

type AtLeastOne<T, Keys extends keyof T> = { [K in Keys]-?: Required<Pick<T, K>> }[Keys];
type Wrapped<U> = [U] extends [readonly unknown[]] ? Elem<U> : U;
type Elem<A extends readonly unknown[]> = A[number];
declare function extract<T extends Record<string, string>, const U extends readonly (keyof T)[]>(t: T, u: U): Pick<T, U[number]>;
type PickDone<D> = D extends { done?: 1; fail?: 2 } ? Pick<Required<D>, 'done' | 'fail'> : never;

interface Action { type: string }
interface Thunk<S> {
    <R>(thunk: (s: S) => R): R;
    <X extends Action>(action: X): X;
}
interface Dispatch { <T extends Action>(action: T, ...extra: any[]): T }
declare const d: Dispatch;
const many: Thunk<unknown> = d;
