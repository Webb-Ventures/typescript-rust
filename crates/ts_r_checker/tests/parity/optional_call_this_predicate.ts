// `ref?.method()` truthy rules out a nullish `ref` and also applies `method`'s
// `this is T` predicate.
type N = { type: string };
type Block = { type: "Block"; body: N[]; directives: string[] };
type Arrow = { type: "Arrow" };
declare class NodePath<T = N> {
  node: T;
  parentPath: T extends Block ? null : NodePath;
  isBlockStatement(props?: object | null): this is NodePath<Block>;
  isArrow(props?: object | null): this is NodePath<Arrow>;
}
export function f(path: NodePath<N>) {
  const parentBody = path.parentPath;
  const parentFunc = parentBody?.parentPath;
  if (
    parentBody?.isBlockStatement() &&
    parentFunc?.isArrow() &&
    parentBody.node.body.length === 1 &&
    parentBody.node.directives.length === 0
  ) {
    return;
  }
}
export function g(p: NodePath | null) {
  if (p?.isBlockStatement()) {
    return p.node.body;
  }
  return null;
}
export function h(p: NodePath | undefined) {
  if (!p?.isBlockStatement()) {
    return null;
  }
  return p.node.directives;
}
export function k(p: NodePath | undefined) {
  if (p?.isArrow()) {
    return p.node.body;
  }
  return null;
}
