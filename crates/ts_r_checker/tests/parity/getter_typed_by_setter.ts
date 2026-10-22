// An unannotated getter has its setter's annotated type, in classes and object literals;
// its returns are checked against it at the return statement.
const o2 = {
  get p1() { return 0; },
  set p1(value: string) {},
};
const v: number = o2.p1;
class C {
  get p1() { return 0; }
  set p1(value: string) {}
}
const o3 = {
  get p1(): string { return 0; },
};
