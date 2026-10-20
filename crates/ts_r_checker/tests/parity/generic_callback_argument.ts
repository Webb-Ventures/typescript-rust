// A generic function passed where a non-generic callback is expected is instantiated in
// the callback's context before inferring from it (domains/billing purchases.ts).
interface Pack { kind: 'pack'; credits: number }
interface Plan { kind: 'plan'; days: number }
interface Offer<P extends Pack | Plan> { product: P; price: number }
declare const packs: Pack[]
declare const plans: Plan[]
const offer = <P extends Pack | Plan>(product: P): Offer<P>[] => [{ product, price: 1 }]
const a: Offer<Pack>[] = packs.flatMap(offer)
const b: Offer<Plan>[] = plans.flatMap(offer)
const c: Offer<Plan>[] = packs.flatMap(offer)
const ids = <T>(x: T): T => x
const d: string[] = [1, 2].map(ids)
