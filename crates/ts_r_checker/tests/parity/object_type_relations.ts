declare const o: object
const f: { [k: string]: unknown } = o
const g: Record<string, any> = o
interface I { a: number }
declare const i: I
const h: Record<string, any> = i
const j: Record<string, unknown> = i
const k: { [k: number]: any } = i
const c: { a: string } = o
const x: Record<string, any> = "s"
const y: Record<string, any> = 1
const z: Record<string, any> = () => 1
class C { a = 1 }
const w: Record<string, any> = new C()
const a: {} = o
const b: Object = o
const d: { a?: string } = o
const e: { toString(): string } = o
Object.entries(o)
const s1: { length: number } = "s"
const s2: { [k: number]: string } = "s"
