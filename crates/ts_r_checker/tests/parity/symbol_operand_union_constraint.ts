function getKey1<S extends symbol>(key: S) {
    let s2!: S | string;
    `${s2}`;
    s2 + '';
    +s2;
    let s3!: S | number;
    -s3;
}
export {};
