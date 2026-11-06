// Pattern literal discriminants narrow by a string literal they match, and two template
// literal types are comparable unless their leading or trailing texts disagree.
type Action =
  | { type: `${string}_REQUEST` }
  | { type: `${string}_SUCCESS`; response: string };

export function reducer(action: Action) {
  if (action.type === "FOO_SUCCESS") {
    const t: 1 = action.type;
    return action.response;
  }
  return "";
}

export function compare(x: `foo-${string}`, y: `${string}-bar`, z: `baz-${string}`) {
  if (x === y) {
    return 1;
  }
  if (x === z) {
    return 2;
  }
  return 0;
}
