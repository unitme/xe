# xe

```sh
cargo run -- play.txt
cargo run -- play.txt --start 12
cargo run -- play.txt --mode random
```

Modes:
- `sequential`
- `random`
- `loop-playlist`
- `loop-song`

`--start` is 1-based for the user.

## Controls

- `Enter`: play selected track
- `Space` / `p`: pause or resume
- `n` / Right: next
- `b` / Left: previous
- Up / Down: move selection
- `m`: cycle playback mode
- `s`: stop
- `+` / `-`: volume
- media keys: play/pause, next/previous, raise/lower/mute volume
- `q` / Ctrl-C: quit
