use crate::assert_go;

#[test]
fn bare() {
    assert_go!(
        r#"
fn go() {
  panic
}
"#,
    );
}
//
// #[test]
// fn panic_as() {
//     assert_js!(
//         r#"
// fn go() {
//   let x = "wibble"
//   panic as x
// }
// "#,
//     );
// }
//
// #[test]
// fn bare_typescript() {
//     assert_ts_def!(
//         r#"
// pub fn go() {
//   panic
// }
// "#,
//     );
// }
//
// #[test]
// fn as_expression() {
//     assert_js!(
//         r#"
// fn go(f) {
//   let boop = panic
//   f(panic)
// }
// "#,
//     );
// }
//
// #[test]
// fn pipe() {
//     assert_js!(
//         r#"
// fn go(f) {
//   f |> panic
// }
// "#,
//     );
// }
//
// #[test]
// fn sequence() {
//     assert_js!(
//         r#"
// fn go(at_the_disco) {
//   panic
//   at_the_disco
// }
// "#,
//     );
// }
//
// #[test]
// fn case() {
//     assert_js!(
//         r#"
// fn go(x) {
//   case x {
//     _ -> panic
//   }
// }
// "#,
//     );
// }
