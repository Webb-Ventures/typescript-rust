// An intersection type argument whose member is the conditional's check type is narrowed
// by the extends type, also inside an outer conditional over a type parameter.
interface A { type: "A"; b: B | null; list: A[] }
interface B { type: "B"; a?: A }
type N = A | B;
type TD<Nd extends N | N[], P extends unknown[]> = Nd extends any ? P : never;
type T1<Nd extends N | N[], P extends unknown[]> = P extends [infer K, ...infer R]
  ? K extends keyof Nd
    ? Nd[K] extends N | N[] | null | undefined
      ? TD<Nd[K] & {}, R>
      : never
    : never
  : never;
type T2<Nd extends N | N[], K extends keyof Nd> = Nd[K] extends N | N[] | null | undefined ? TD<Nd[K] & {}, []> : never;
type T4<Nd extends N | N[], K> = K extends keyof Nd ? Nd[K] extends N | N[] | null | undefined ? TD<Nd[K] & {}, []> : never : never;
type T5<Nd extends N | N[], K> = K extends keyof Nd ? Nd[K] extends N | N[] ? TD<Nd[K], []> : never : never;
type T7<Nd extends N | N[], K> = K extends keyof Nd ? Nd[K] extends N | N[] | null | undefined ? TD<{} & Nd[K], []> : never : never;
type T8<Nd extends N | N[], K extends keyof Nd> = Nd[K] extends N | N[] | null | undefined ? TD<{} & Nd[K], []> : never;
