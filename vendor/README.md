# Vendored crates

## whisper-rs 0.16.0

The crates.io release, with one function added in `src/whisper_params.rs`:
`FullParams::set_carry_initial_prompt`, which sets whisper.cpp's
`carry_initial_prompt`. whisper.cpp then gives the initial prompt (the names
and words Minutes tells it to expect) to every 30-second window instead of
the first one only. On 74 minutes of a French meeting, misspelled work terms
went from 13 to 2 (`bench/JOURNAL.md`).

Remove this copy, and the `[patch.crates-io]` entry in `Cargo.toml`, once a
whisper-rs release has the setter.
