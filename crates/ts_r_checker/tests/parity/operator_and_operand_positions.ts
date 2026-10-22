// TS2447 points at the operator token, named as written; TS2703 points at the operand
// inside its parentheses; a parameter property's bad default points at the parameter,
// modifiers included.
declare let a: boolean
a ^= a
const r = a & a
declare const n: number
delete (n + n)
class C {
  constructor(public x: string = 1) {}
}
export { r, C }
