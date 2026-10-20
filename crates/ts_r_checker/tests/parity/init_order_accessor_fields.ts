class C1 {
    a = this.x;
    accessor x = 1;
    b = this.x;
    accessor y: number;
    c = this.y;
    static { C1.z; }
    static accessor z = 1;
    static { this.w = 1; }
    static accessor w: number;
    d = this.v;
    accessor v!: number;
}
export {};
