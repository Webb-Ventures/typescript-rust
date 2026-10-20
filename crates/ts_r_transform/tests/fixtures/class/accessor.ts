// @target: es2022
export class A {
  static accessor sa = 1;
  accessor #pa = 2;
  accessor ["c" + 1] = 3;
  accessor "str" = 4;
  accessor 5 = 5;
  accessor x;
}
export default class {
  constructor(private q: number) {}
}
