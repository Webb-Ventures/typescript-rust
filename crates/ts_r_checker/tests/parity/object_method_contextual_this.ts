// In an object literal method, the contextual signature's `this` parameter types `this`.
interface Ctx { isNot?: boolean; promise?: string }
type Fn<C extends Ctx = Ctx> = { (this: C, actual: any, ...expected: any[]): void };
type Obj = { [name: string]: Fn };
export const m: Obj = {
  toBe(received: unknown) {
    const n: boolean | undefined = this.isNot;
    const p: string = this.promise!;
  },
};
type Obj2 = { toBe: Fn };
export const m2: Obj2 = {
  toBe(received: unknown) {
    const n: boolean | undefined = this.isNot;
  },
};
export const m3: Obj = {
  toBe(received: unknown) {
    const s: string = this.isNot;
  },
};
