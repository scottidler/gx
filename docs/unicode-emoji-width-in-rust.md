# Unicode Emoji Width Calculation in Rust

I am writing a program that writes unicode emoji characters to the terminal in a rust program. I want to calculate the widths of unicode emojis and normal ascii characters so that I can align columns of text correctly. This document provides proven approaches, documentation, and code examples for handling Unicode width calculations in Rust.

## Proven Approaches & Documentation

### 1) Quick & Standard: `unicode-width`

- **What it is**: Classic Rust crate that returns display width per `char` / `str` (East Asian width rules, emoji heuristics).
- **Docs**: [docs.rs page and repo](https://docs.rs/unicode-width/)
- **Caveat**: May not perfectly match all terminals (combining marks, complex scripts). The crate explicitly calls this out.
- **How**:

```rust
// Cargo.toml
// unicode-width = "0.1"

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn width_cell(s: &str) -> usize {
    s.width() // counts terminal cells; "😊" is often 2
}

fn width_char(c: char) -> usize {
    c.width().unwrap_or(0)
}
```

### 2) ANSI-aware Convenience: `console::measure_text_width`

- **What it is**: Handles ANSI color codes (strips them) and uses Unicode width under the hood—nice for TUI/CLI output.
- **Docs**: [`console` crate + function page + GitHub](https://docs.rs/console/)
- **How**:

```rust
// Cargo.toml
// console = "0.15"

use console::measure_text_width;

let w = measure_text_width("green ✅");
```

### 3) Grapheme-cluster Aware: `unicode-segmentation` (+ width)

- **What it is**: Iterate Unicode grapheme clusters (UAX #29)—useful when you need to treat multi-codepoint emoji (ZWJ sequences, skin tones) as one unit, then sum widths.
- **Docs**: [crate docs + GitHub; SO explainer](https://docs.rs/unicode-segmentation/)
- **How**:

```rust
// Cargo.toml
// unicode-segmentation = "1"
// unicode-width = "0.1"

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn display_width_grapheme_aware(s: &str) -> usize {
    s.graphemes(true).map(|g| g.width()).sum()
}
```

### 4) "All-in-one" Width Calculators that Handle Emoji Sequences

- **`string_width` crate**: Advertises correct handling of emoji sequences (👨‍👩‍👧‍👦), flags (🇺🇸), keycaps (1️⃣), combining marks, zero-width chars, and ANSI. Good for terminals. [Docs.rs](https://docs.rs/string_width/)

```rust
// Cargo.toml
// string-width = "0.3"

use string_width::StringWidth;
let w = "👨‍👩‍👧‍👦 + 🇺🇸".display_width(); // e.g., 2 + 2 + 1 spaces etc.
```

- **`unicode-display-width` crate**: Focused on Unicode 15.1, claims grapheme-correct column counts. (Newer project; review for your needs.) [GitHub](https://github.com/unicode-rs/unicode-display-width)

### 5) Deeper Background & Alternatives

- **Blog**: ["Calculating String length and width – Fun with Unicode (Rust)"](https://www.tomdebruijn.com/posts/rust-string-length-width-calculations/) — explains ZWJ and why emoji sequences break naïve counts.
- **General "wcwidth" background & pitfalls** — terminal cell widths, full-/zero-width, and gotchas. (Python-centric but concepts carry over.) [jeffquast.com](https://jeffquast.com/post/terminal_wcwidth_pitfalls/)
- **Reality check**: Emoji width bugs often come from graphemes vs codepoints vs display cells—expect terminal differences. [Hacker News discussion](https://news.ycombinator.com/item?id=28512988)
- **Exploratory**: [`runefix-core`](https://github.com/runefix/runefix) (segments by "atoms" = layout units) for TUIs. Not standard yet, but interesting if you're building a renderer.

## What to Use When

- **Just need columns to line up (no ANSI), and "mostly right"**: `unicode-width`. [Docs.rs](https://docs.rs/unicode-width/)
- **You print colored text / ANSI codes**: `console::measure_text_width`. [Docs.rs](https://docs.rs/console/)
- **You truncate/pad without breaking emoji clusters**: `unicode-segmentation` + sum widths per grapheme. [Docs.rs](https://docs.rs/unicode-segmentation/)
- **You want the fewest surprises with emoji sequences & flags**: try `string_width` (and validate against your target terminals). [Docs.rs](https://docs.rs/string_width/)

## Tiny Alignment Helper (Combines the Ideas)

```rust
// Cargo.toml
// unicode-segmentation = "1"
// unicode-width = "0.1"

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn display_width(s: &str) -> usize {
    s.graphemes(true).map(|g| g.width()).sum()
}

fn pad_right(s: &str, target: usize) -> String {
    let w = display_width(s);
    if w >= target { s.to_string() } else { format!("{s}{:width$}", "", width = target - w) }
}

fn main() {
    let rows = vec!["OK", "😊", "👨‍👩‍👧‍👦", "🇺🇸", "À"]; // last is 'A' + combining acute
    for r in rows {
        println!("{} | {}", pad_right(r, 6), display_width(r));
    }
}
```

## Terminal Compatibility Notes

If you tell me your target terminals (e.g., iTerm2, macOS Terminal, GNOME Terminal, Windows Terminal) I can suggest a test matrix; some terminals differ on obscure sequences.

## Summary

Terminal "width" is trickier than it looks with emoji, ZWJ sequences, and CJK. Here are solid docs/blogs and the exact crates people use, plus how to use them. The most robust approach combines `unicode-segmentation` for proper grapheme handling with `unicode-width` for width calculations, but simpler solutions like the `string_width` crate may be sufficient for many use cases.