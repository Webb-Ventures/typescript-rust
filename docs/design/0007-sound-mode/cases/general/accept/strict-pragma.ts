// @ts-r strict
// At the strict level, only S-01, S-04 and S-10 apply: casts and ! are allowed.
interface User {
  readonly id: number;
}
const el = document.getElementById("app")!;
const u = ({ id: 1 } as unknown) as User;
export const out: string = el.tagName + u.id;
