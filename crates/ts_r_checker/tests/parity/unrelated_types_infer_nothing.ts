// Structural inference skips types that each have a required property the other lacks.
declare class Op<T> {
  private _value: T | Op<T>;
  readonly kind: string;
  get value(): T;
}
declare function equal<T>(value: T | Op<T>): Op<T>;
export class Uuid {
  private value: string;
  constructor(value: string) {
    this.value = value;
  }
  getValue(): string {
    return this.value;
  }
}
declare const id: Uuid;
export const a = equal(id);
export const n: number = a;
declare const like: { value: number; kind: string };
export const b = equal(like);
export const m: number = b;
