# East Asian encoding fixtures (M24)

One small file per East Asian encoding Nimbus offers with the
`east-asian` cargo feature. Each holds two lines of text in its language,
with characters that need two bytes (four in GB18030), and was made by
Python's codecs from the text in `crates/app/src/document.rs`'s
`east_asian_fixtures_round_trip` test (`cp932` for Shift_JIS, which is
what the WHATWG standard and `encoding_rs` mean by Shift_JIS).

Run the tests that use them with:

```sh
cargo test -p nimbus --features east-asian east_asian
```
