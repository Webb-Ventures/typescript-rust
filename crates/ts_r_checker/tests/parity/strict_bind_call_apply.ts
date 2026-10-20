// Under `strictBindCallApply` (on with `strict`), `call`/`apply`/`bind` come from
// `CallableFunction` (or `NewableFunction` for construct-only types) and are typed.
declare const f: (a: number) => string
declare const C: new (a: number) => { x: number }
export const r1: string = f.call(undefined, 1)
export const r2: number = f.call(undefined, 1)
export const r3 = f.call(undefined, 'no')
export const r4: string = f.apply(undefined, [1])
export const r6: () => string = f.bind(undefined, 1)
export const r8 = C.bind(undefined, 1)
export const r9: number = new r8().x
export const r10: string = (f as (...a: unknown[]) => unknown).apply(undefined, [])
export const r11 = f.toString()
export const r12: string = f.name
