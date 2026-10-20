declare module 'virtual:icons/*' {
  const component: (props: { size?: number }) => unknown;
  export default component;
}
declare module 'shorthand';
export declare function overload(x: string): string;
export declare function overload(x: number): number;
export declare class Emitter<Events extends Record<string, unknown[]>> {
  private listeners;
  on<E extends keyof Events>(event: E, fn: (...args: Events[E]) => void): this;
  static readonly VERSION: string;
}
export type { Emitter as default };
declare const VERSION: `${number}.${number}.${number}`;
export as namespace Lib;
