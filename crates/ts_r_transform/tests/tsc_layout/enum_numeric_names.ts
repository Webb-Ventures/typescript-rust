// @target: es2022
// Numeric, bigint and private member names (errors) and a missing member.
enum Nums {
    1.0,
    11e-1,
    "13e-1",
    0xF00D
}
enum E { 0n = 0 }
enum P { #x }
enum S { , }
enum C { [1] = 2 }
