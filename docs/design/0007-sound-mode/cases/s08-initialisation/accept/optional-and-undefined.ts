// Optional fields and fields whose type includes undefined count as initialised.
class TreeNode {
  label?: string;
  parent: TreeNode | undefined;
  readonly children: TreeNode[] = [];
  constructor(label: string | undefined) {
    if (label !== undefined) this.label = label;
  }
}
export const root: boolean = new TreeNode(undefined).parent === undefined;
