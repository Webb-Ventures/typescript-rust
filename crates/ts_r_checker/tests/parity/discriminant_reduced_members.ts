// A union member emptied by a conflicting discriminant takes no part in deciding whether a
// property is a discriminant, and a `for...of` variable over an explicitly typed array is
// an explicitly typed assertion target.
type RuntimeValue =
  | { type: "number"; value: number }
  | { type: "string"; value: string };

export function onlyNumber(x: RuntimeValue & { type: "number" }) {
  if (x.type === "number") {
    return x.value;
  } else {
    return x.value;
  }
}

declare class Test {
  isTest2(): this is Test2;
  assertIsTest2(): asserts this is Test2;
}
declare class Test2 extends Test {
  z: string;
}

export function loop(items: Test[]) {
  for (const item of items) {
    item.assertIsTest2();
    item.z;
  }
}
