// From @opentelemetry/sdk-trace and expect-type: a generic rest (`...args: A`) infers
// from the other signature's rest as a whole when two generic signatures are related,
// and a deferred conditional's constraint reads its rest-position `infer` as `unknown[]`.
interface Context { x: number }
interface ContextManager {
    with<A extends unknown[], F extends (...args: A) => ReturnType<F>>(context: Context, fn: F, thisArg?: ThisParameterType<F>, ...args: A): ReturnType<F>;
}
declare class StackContextManager implements ContextManager {
    with<A extends unknown[], F extends (...args: A) => ReturnType<F>>(context: Context | null, fn: F, thisArg?: ThisParameterType<F>, ...args: A): ReturnType<F>;
}
type UnknownFunction = (...args: unknown[]) => unknown;
type Post<F> = F extends { (...args: infer A1): infer R1; (...args: infer A2): infer R2 } ? ((...p: A1) => R1) | ((...p: A2) => R2) : never;
type InferFunctionType<F extends (...args: any) => any> = F;
type Select<U extends UnknownFunction, Args extends unknown[]> = U extends InferFunctionType<infer Fn> ? (Args extends Parameters<Fn> ? Fn : never) : never;
type Narrowed<F, Args extends unknown[]> = Select<Post<F>, Args>;
export {};
