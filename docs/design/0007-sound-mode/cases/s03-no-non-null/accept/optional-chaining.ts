// Optional chaining keeps undefined in the type.
interface ListNode {
  readonly value: number;
  readonly next?: ListNode;
}
function secondValue(n: ListNode): number | undefined {
  return n.next?.value;
}
export const s: number | undefined = secondValue({ value: 1 });
