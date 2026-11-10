// Destructured bindings narrow each other through a generic parent's constraint, a rest
// parameter's tuple union, and keep the narrowing of the property read where they are
// declared (dependentDestructuredVariables).
type Action = { kind: "A"; payload: number } | { kind: "B"; payload: string };
type Action2 =
  | { kind: "A"; payload: number | undefined }
  | { kind: "B"; payload: string | undefined };
type Args = ["A", number] | ["B", string];

export function generic<T extends Action>({ kind, payload }: T) {
  if (kind === "A") {
    payload.toFixed();
  }
  if (kind === "B") {
    payload.toUpperCase();
  }
}

export function declaredAfterCheck(action: Action2) {
  if (action.payload) {
    const { kind, payload } = action;
    if (kind === "A") {
      payload.toFixed();
    }
    if (kind === "B") {
      payload.toUpperCase();
    }
  }
}

export function rest(...[kind, data]: Args) {
  if (kind === "A") {
    data.toFixed();
  }
  if (kind === "B") {
    data.toUpperCase();
  }
}

export function stillChecked(action: Action2) {
  const { kind, payload } = action;
  if (kind === "A") {
    payload.toFixed();
  }
}
