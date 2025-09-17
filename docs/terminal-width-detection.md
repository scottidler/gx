# Terminal Width Detection for Emojis

## The Problem

Is there a way to detect if your terminal is using a different width for some emojis vs what the string_width library says they should be?

## The Solution

Yep—measure what your terminal actually does. The trick is to print a string at a known cursor position, then ask the terminal where the cursor ended up (CSI "cursor position report", aka DSR 6). The delta in columns is the _real_ display width on that terminal; you can compare that to what `string_width` says. Most terminals support CSI `\x1b[6n` and reply with `\x1b[row;colR`.

Here's a small Rust probe that uses `crossterm` (which issues the CPR query for you) and compares against `string_width`:

```rust
# // Cargo.toml
# // crossterm = "0.27"
# // string-width = "0.3"

use crossterm::{
    cursor::{position, MoveTo},
    terminal::{self, enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    execute, queue,
};
use std::io::{stdout, Write};
use string_width::string_width;

fn measure_terminal_width(s: &str) -> std::io::Result<u16> {
    let mut out = stdout();

    enable_raw_mode()?;                          // allow reading CPR response
    execute!(out, EnterAlternateScreen)?;        // clean slate
    queue!(out, Clear(ClearType::All), MoveTo(0, 0))?;
    out.flush()?;

    let (start_col, start_row) = position()?;
    write!(out, "{}", s)?;                       // print the test string
    out.flush()?;
    let (end_col, end_row) = position()?;       // ask terminal where we are now

    let (cols, _rows) = terminal::size()?;
    let width = if end_row == start_row {
        end_col.saturating_sub(start_col)
    } else {
        // handle wrap just in case (keep test strings shorter than `cols` to avoid this)
        (cols - start_col) + ((end_row - start_row - 1) * cols) + end_col
    };

    execute!(out, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(width)
}

fn main() -> std::io::Result<()> {
    let samples = ["😀", "🚀", "🇺🇸", "🏃", "🅰️", "©️", "🅰\u{fe0f}"];
    for s in &samples {
        let term = measure_terminal_width(s)?;
        let lib = string_width(s) as u16;
        println!("{:<12} lib={} term={} {}", s, lib, term, if lib==term { "OK" } else { "MISMATCH" });
    }
    Ok(())
}
```

## Key Points

- **string_width** claims careful handling of emoji ZWJ sequences, flags, keycaps, combining marks, and ANSI—use it as your "expected" value. Then this probe tells you if your **terminal** disagrees.

- **crossterm::cursor::position()** is a convenient wrapper that requests & reads CPR for you (cross-platform).

## What to do with mismatches

- **Warn or auto-tune**: if enough samples mismatch, mark the terminal as "non-standard widths" and prefer measured widths for padding/truncation.
- **Keep tests < terminal width**: avoid wrapping (or use the wrap-aware branch shown above).
- **Document terminal settings**: Some terminals expose toggles that _change_ width rules (e.g., iTerm2 "Use Unicode Version 9 Widths" and "Treat ambiguous-width characters as double width"). Users flipping these will change results.

## Alternative (manual CPR)

If you don't want a dependency, you can write `\x1b[6n` to stdout, read the `\x1b[{row};{col}R` reply from stdin, and compute the delta yourself—same idea, just more plumbing. (That's the ECMA-48 / xterm CPR behavior.)

If you want, I can adapt this into a tiny "calibrate widths" helper that runs at startup, caches the result, and falls back to `string_width` when CPR isn't supported.
