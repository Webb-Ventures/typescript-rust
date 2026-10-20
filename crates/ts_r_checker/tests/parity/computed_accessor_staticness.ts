// A computed setter takes its type from the getter of the same staticness.
declare const k: unique symbol;
class C {
  static get [k](): string { return ''; }
  get [k](): number { return 1; }
  set [k](v) { const n: number = v; }
}
class D {
  get [k](): number { return 1; }
  static get [k](): string { return ''; }
  static set [k](v) { const s: string = v; }
}
