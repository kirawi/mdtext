`mdtext` is a fully incremental/streaming markdown parser created to be used to render LLM streaming generation. Unlike many other projects with similar functionality, the library is human-written for the sake of performance, correctness, and architectural robustness. It is about as fast as `pulldown-cmark` on a full document parse.

For incremental parsing, `mdtext` stays linear while `pulldown-cmark` becomes quadratic. Here is the result of an ad hoc (take with a grain of salt) comparison:

```
mdtext incr            :   9.472ms  (  165886 events,    1.00 MiB re-parsed)
     mem:    12213 allocs,      2.84 MiB allocated,   65.16 KiB peak live
     checkpoints: [ 951µs    2ms    3ms    4ms    5ms    6ms    7ms    8ms    9ms]
pcm reparse            :    5.143s  (75661195 events,  512.50 MiB re-parsed)
     mem:  8053584 allocs,  12196.50 MiB allocated, 13517.72 KiB peak live
     checkpoints: [  51ms  207ms  467ms  830ms     1s     2s     3s     3s     4s]
String append-only     :  29.094µs  (       0 events,    1.00 MiB re-parsed)
     mem:       12 allocs,      2.00 MiB allocated, 1024.16 KiB peak live
     checkpoints: [   4µs    6µs    9µs   11µs   21µs   22µs   24µs   26µs   28µs]
```

Current full parse benchmarks on random corpus:

```
mdtext/parse            time:   [7.8768 ms 7.8866 ms 7.8956 ms]
                        thrpt:  [197.90 MiB/s 198.12 MiB/s 198.37 MiB/s]

pulldown_cmark/parse    time:   [8.4118 ms 8.4250 ms 8.4385 ms]
                        thrpt:  [185.16 MiB/s 185.46 MiB/s 185.75 MiB/s]
```

Currently, the library implements and has been tested against both CommonMark and GitHub-Flavored Markdown for full compliance with the respective specifications. Note, however, that parsers (including the reference parsers) diverge from the official specification at times. Where such conflicts occured, `cmark`/`cmark-gfm`'s semantics were chosen. Additionally, the library was designed to avoid backtracking. Accordingly, tight/loose list differentiation and reference links are *currently* not implemented as they require unbounded lookahead or backtracking.

While the library itself is human-written, note that the following were almost or entirely AI-generated:
- The static website
- `main.rs`/`web.rs`
- `tests/` (though test cases are themselves human-verified as correct)

There may be bugs with them. They exist purely for demonstrative purposes and may receive a human look-over at a later date, but my current priority is to work on the projects that depended on this library being made.

Please check out the demo at https://kirawi.github.io/mdtext/
