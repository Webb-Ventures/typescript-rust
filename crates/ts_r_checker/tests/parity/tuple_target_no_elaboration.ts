// A tuple target is not elaborated with missing properties: only the head message.
declare const ia: IArguments
export const a: [] = ia
export const b: [number] = ia
export const c: [number] = { a: 1 }
declare const o: { length: 1 }
export const d: [number] = o
export const e: readonly [number] = o
declare function t(x: [number]): void
t(o)
