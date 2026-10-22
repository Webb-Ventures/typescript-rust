type Simplify<T> = T extends any[] | Date ? T : { [K in keyof T]: T[K] } & {};
type PartialIf<C, T> = C extends true ? Partial<T> : T;
type CCB<TContext, F> = PartialIf<object extends TContext ? true : false, { createContext: F }>;
interface CallerOverride<TContext> { (o: TContext): void }
declare function caller<TContext>(config: Simplify<{ x?: boolean } & CCB<TContext, () => TContext>>): CallerOverride<TContext>;
declare function use(c: CallerOverride<{ foo: string }>): void;
use(caller({ createContext: () => ({ foo: '' }) }));
const n: number = caller({});
