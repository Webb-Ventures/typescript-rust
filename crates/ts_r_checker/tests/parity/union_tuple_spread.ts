// Destructuring a union of tuples indexes each member, and a spread tuple's optional elements
// are arguments that may be undefined (private-app infra-project).
type Stack = 'staging' | 'production'
declare function of(s: Stack): void
for (const [stack, branch] of [['staging', 'staging'], ['production', 'main']] as const) { of(stack); of(branch) }
declare const t: readonly ['a', 'b'] | readonly ['c', 'd'] | readonly ['e']
const [x, y] = t
export const z: 'a' | 'c' = x
export const w: 'b' = y
const github = (a: string[], r: Record<string, string> = {}) => ({ a, r })
export const check = (...args: Parameters<typeof github>) => github(...args)
declare function g1(a: number): void
declare function g3(a: number, b: string): void
declare const t2: [number, string?]
g1(...t2)
g3(...t2)
