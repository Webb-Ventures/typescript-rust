// A callback of an overloaded generic method whose first overload takes a type predicate
// (`find`, `filter`): inferring a predicate from the arrow's body while the call resolves
// must not fix its parameter as an implicit `any`.
interface M {
  userId: string
  template: string
}
declare const members: M[]
export const a = members.find((m) => m.userId === 'x')
export const b = members.filter((m) => m.template === 'x')
export const c = members.find((m) => m.nope === 'x')
export const d = members.filter((m) => m.nope2 === 'x')
declare const ro: readonly M[]
export const e = ro.find((m) => m.nope3 === 'x')
export const f = Object.keys({ a: 1 }).filter((k) => k.nope4)
