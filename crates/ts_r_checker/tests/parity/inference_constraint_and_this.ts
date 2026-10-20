// An inferred type parameter is checked against its constraint with its own references
// in the constraint replaced by the inference.
declare function F<C extends (readonly [] | readonly string[]) & (number extends C["length"] ? readonly [] : unknown)>(
  c: C,
  r: { [K in keyof C]: string },
): void
F(["a", "b"], ["c", "d"])
declare function G<C extends (readonly [] | readonly string[]) & (number extends C["length"] ? readonly [] : unknown)>(c: C): C
const g = G(["a", "b"])

// `this` parameters infer like other parameters (`infer This` in a conditional type).
interface Ctx { addWatchFile(f: string): void }
type MakeAsync<F> = F extends (this: infer This, ...parameters: infer Args) => infer R ? (this: This, ...parameters: Args) => R | Promise<R> : never
type ObjectHook<T, O = {}> = T | ({ handler: T } & { order?: "pre" | null } & O)
interface Hooks {
  buildStart: (this: Ctx, options: string) => void
  load: (this: Ctx, id: string) => string
}
type PluginHooks = { [K in keyof Hooks]: ObjectHook<K extends Exclude<keyof Hooks, "load"> ? MakeAsync<Hooks[K]> : Hooks[K]> }
interface Plugin extends Partial<PluginHooks> { name: string }
export const p: Plugin = {
  name: "x",
  buildStart() {
    this.addWatchFile("a")
  },
  load(id) {
    this.addWatchFile(id)
    return id
  },
}
declare const a1: MakeAsync<Hooks["buildStart"]>
const t1: string = a1
export { g, t1 }
