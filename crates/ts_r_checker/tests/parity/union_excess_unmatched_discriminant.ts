// A discriminant whose value no union member accepts selects nothing for the excess
// property check: `{ a: null }` against `W[] | W` checks against the whole union and
// reports the property's type, not an excess property in `W[]`.
class Category { id!: number }
type W1 = { a?: Category | boolean }
const w1: W1[] | W1 = { a: null }
class Op<T> { v!: T }
type Prop<P> = P extends string ? P | Op<P> : P extends object ? Where<P> | Where<P>[] | Op<any> | boolean | P : P
type Where<E> = { [K in keyof E]?: Prop<NonNullable<E[K]>> }
class Post { id!: number; category!: Category | null }
interface Opts<E> { where?: Where<E>[] | Where<E> }
declare function find<E>(o: Opts<E>): void
find<Post>({
  where: {
    category: null,
  },
})

// Members without the discriminant stay candidates.
const x1: { kind: "a" } | { x: number } = { kind: "a", x: 1 }
const x3: { kind: "a" } | { kind: "b", y: 1 } | { x: number } = { kind: "b", x: 1 }
const x4: { kind?: "a" | true } | { x: number }[] = { kind: false, x: 1 }

// A property checked against the union's property types keeps the head and is reported
// at the object (the best matching member, the last of equal overlap, lacks it).
const x2: { kind: "a" } | { x: number } = { kind: "b", x: 1 }
const x5: { kind: "a" } | { x: number } = { kind: "b" }
const x6: { kind: "a" } | { x: number, y: 1 } = { kind: "b", x: 1 }
export { w1, x1, x2, x3, x4, x5, x6 }
