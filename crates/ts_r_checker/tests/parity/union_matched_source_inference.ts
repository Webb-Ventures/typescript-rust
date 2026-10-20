// A source matched entirely by other union members still infers to the naked type
// parameter, at low priority; generic methods of one declaration relate unified first, then by inference.
interface Box<A> { a: A }
declare const s: string;
declare const sn: string | number;
declare const bn: Box<number>;
declare function g<K>(e: K | string | symbol): K;
export const g1: boolean = g(s);
export const g2: boolean = g(sn);
declare function f<T>(x: T | undefined): T;
export const f1: boolean = f(undefined);
declare function h<T>(x: T | Box<number>): T;
export const h1: boolean = h(bn);
declare function v<T>(x: T | string, y: T): T;
export const v1: boolean = v(s, 1);

// Node's typed EventEmitter: `K` is inferred from a `string` event name.
type EventMap<T> = Record<keyof T, any[]> | DefaultEventMap;
type DefaultEventMap = [never];
type Key<K, T> = T extends DefaultEventMap ? string | symbol : K | keyof T;
type Listener<K, T, F> = T extends DefaultEventMap ? F : (
    K extends keyof T ? (
            T[K] extends unknown[] ? (...args: T[K]) => void : never
        )
        : never
);
type Listener1<K, T> = Listener<K, T, (...args: any[]) => void>;
declare class EE<T extends EventMap<T> = DefaultEventMap> {
  on<K>(eventName: Key<K, T>, listener: Listener1<K, T>): this;
}
type EM = Record<string | symbol, any[]>;
class Sdk<M extends EM = EM> extends EE<M> {}
class Browser extends Sdk {
  static Events = { Disconnected: 'disconnected' };
  go() {
    this.on(Browser.Events.Disconnected, () => {});
  }
}
declare function waitFor(e: EE, ev: string): void;
export class Frame extends Sdk<{ nav: [number] }> {
  go() { waitFor(this, 'nav'); }
}

// Generic methods with differently constrained type parameters.
interface E3 { on<K>(e: K | "nav"): void }
interface E4 { on<K>(e: K | number): void }
declare const x3: E3;
export const y3: E4 = x3;
interface E5 { on<K extends string>(e: K | "nav"): void }
interface E6 { on<K extends string>(e: K | number): void }
declare const x5: E5;
export const y5: E6 = x5;
