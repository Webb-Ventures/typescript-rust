export const a = new.target;
export const b = () => new.target;
export function f() {
  return () => new.target;
}
export const g = function () {
  return new.target;
};
export class C {
  constructor() {
    void new.target;
  }
  m() {
    return new.target;
  }
  p = () => new.target;
}
export const o = {
  k() {
    return new.target;
  },
};
