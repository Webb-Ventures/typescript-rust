// Types shared by the cases. This file is itself at the sound level and is clean.
export interface Animal {
  readonly name: string;
}
export interface Dog extends Animal {
  readonly kind: "dog";
  bark(): string;
}
export interface Cat extends Animal {
  readonly kind: "cat";
  meow(): string;
}
export interface Role {
  readonly name: string;
}
export interface User {
  readonly id: number;
  readonly name: string;
  readonly email?: string;
  readonly roles: readonly Role[];
}
export type Shape =
  | { readonly kind: "circle"; readonly radius: number }
  | { readonly kind: "square"; readonly side: number };
export type Circle = Extract<Shape, { kind: "circle" }>;
export interface Settings {
  readonly theme: "light" | "dark";
  readonly fontSize: number;
}
export function makeDog(name: string): Dog {
  return { name, kind: "dog", bark: () => "woof" };
}
export function makeCat(name: string): Cat {
  return { name, kind: "cat", meow: () => "meow" };
}
