// A type with extra properties is not a subtype of an object literal type, so a class
// candidate wins over an object literal one; union members that infer only weakly still
// reach the naked type parameter.
declare class Base {
  hasId(): boolean;
}
declare class User extends Base {
  id: number;
  name: string;
}
declare function s5<T>(c: { new (): T }, e: T): T;
export const n5: number = s5(User, { name: "x" });
declare function s6<T>(c: { new (): T }, e: T | { [K in keyof T]?: T[K] }): T;
export const n6: number = s6(User, { name: "x" });
declare function s7<T>(c: { new (): T }, e: T | (T extends object ? { [K in keyof T]?: T[K] } : T)): T;
export const n7: number = s7(User, { name: "x" });
declare function s8<T>(c: T, e: T | (T extends object ? { [K in keyof T]?: T[K] } : T)): T;
export const n8: number = s8({}, { name: "x" });
declare function s9<T>(c: T, e: T | { [K in keyof T]?: T[K] }): T;
export const n9: number = s9({}, { name: "x" });
declare const u: User;
export const n10: number = s9(u, { name: "x" });
export const n11: number = s8(u, { name: "x" });
