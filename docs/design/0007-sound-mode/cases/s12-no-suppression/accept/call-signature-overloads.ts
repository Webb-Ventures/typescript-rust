// Overloads as call signatures of a type have no unchecked implementation.
interface Same {
  (x: string): string;
  (x: number): number;
}
const same: Same = <T extends string | number>(x: T): T => x;
export const s: string = same("a");
