export interface User {
  id: number;
  name: string;
  tags?: string[];
}

export function greet(u: User): string {
  return `hello ${u.name} (${u.tags?.join(', ') ?? 'no tags'})`;
}

const users: User[] = [
  { id: 1, name: 'a' },
  { id: 2, name: 'b', tags: ['x'] },
];

export const names = users.map((u) => greet(u)).filter((s) => s.length > 3);

export class Box<T> {
  constructor(readonly value: T) {}
  map<U>(f: (v: T) => U): Box<U> {
    return new Box(f(this.value));
  }
}

export const doubled: number = new Box(2).map((v) => v * 2).value;
