// Inference to an index signature from a source's implicit index signature: object
// literals, type literals and mapped types; optional properties lose their `undefined`.
type Market = 'AU' | 'NZ'
interface TaxRule { name: string; rate: number }
declare const taxes: Partial<Record<Market, TaxRule>>
for (const [market, tax] of Object.entries(taxes)) { tax.rate; market }
declare const t2: Record<Market, TaxRule>
for (const [, tax] of Object.entries(t2)) { tax.rate }
declare function f<T>(o: { [s: string]: T }): T
declare const o1: { a?: number }
declare const o2: { a?: number | undefined }
declare const o3: { a: number | undefined }
const r1: { [k: string]: number } = o1
const r2: { [k: string]: number } = o2
const r3: { [k: string]: number } = o3
const x1: string = f(o1)
const x2: string = f(o2)
const x3: string = f(o3)
const x4: number = f(taxes)
interface I { a: number }
declare const i: I
const x5: string = f(i)
