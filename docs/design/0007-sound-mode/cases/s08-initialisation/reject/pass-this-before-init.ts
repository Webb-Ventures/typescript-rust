// Registering this before its id is set.
const registry = new Set<object>();
class Entity {
  id: string;
  constructor(id: string) {
    registry.add(this); // @expect TSR0802
    this.id = id;
  }
}
new Entity("a");
export {};
