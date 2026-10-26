export {};
declare global {
  interface Foo {
    bold(): string;
    big(): string;
    dim(): string;
  }
}
// @filename: b.ts
export {};
declare global {
  interface Foo {
    // @ts-ignore
    bold: string;
    // @ts-expect-error
    big: string;
    dim: string;
  }
}
