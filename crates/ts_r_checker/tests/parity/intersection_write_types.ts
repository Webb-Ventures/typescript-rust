// A property of an intersection is written as the intersection of the members' write
// types; a write through a union of literal keys as the intersection of each property's
// write type.
export {};
class One {
  get prop1(): string { return ""; }
  set prop1(s: string | number) { }
  prop2: number = 1;
}
class Two {
  get prop1(): "hello" { return "hello"; }
  set prop1(s: "hello" | number) { }
  get prop2(): string { return ""; }
  set prop2(s: string | 42) { }
}
declare const i: One & Two;
i.prop1;
i.prop1 = 42;
i.prop1 = "hello";
i.prop2;
i.prop2 = 42;
i.prop2 = "hello";
i["prop2"] = "hello";
class Six {
  get prop1(): boolean | number { return 42; }
  set prop1(s: boolean | string) {}
  get prop2(): bigint | number { return 10; }
  set prop2(s: boolean | null) {}
}
declare const s1: Six;
declare const k1: "prop1" | "prop2";
const sv1 = s1[k1];
s1[k1] = 42;
s1[k1] = true;
s1[k1] = "";
s1[k1] = null;
declare const o: { a: string; b: number };
declare const ab: "a" | "b";
o[ab] = "x";
