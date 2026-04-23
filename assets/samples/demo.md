# mdrdr — milestone 2

A *from-scratch* markdown viewer that grows one milestone at a time.

## What renders today

Block elements: headings of six levels, paragraphs, fenced code blocks, bullet and ordered lists, blockquotes, thematic breaks. Inline: `code spans`, **bold**, *italic*, combined ***bold italic***, and [links](https://example.com).

### Third-level heading

Text in a paragraph will **wrap** automatically when it runs past the right margin — try resizing the window and the lines reflow. This sentence is intentionally long so you can watch the word break happen at the natural space boundary rather than the middle of a word, which would look terrible and is the obvious wrong thing to do.

## Lists

Unordered:

- apples, the ordinary sort
- pears, slightly bruised
- oranges with their *italic* peel

Ordered:

1. boil the kettle
2. steep for four minutes
3. drink before it gets cold

## Code

Inline commands like `cargo run --release` flow with the surrounding text. Fenced blocks get their own background and the accent colour:

```rust
fn main() {
    let greeting = "hello, mdrdr";
    println!("{}", greeting);
}
```

## Quotes

> Everything above the primitive layer is written by us.
>
> That is the single rule of the project, and it keeps the dep tree honest.

---

## Greek corner

α β γ δ ε ζ η θ ι κ λ μ ν ξ ο π ρ σ τ υ φ χ ψ ω — rendered straight from DejaVu's glyph coverage, no shaping, no special-casing.

## Images

Inline images land in M4; for now the parser records them and the layout shows a placeholder: ![the sunrise](sunrise.png)

###### Smallest heading

The end. Press PageDown or scroll to see this line when the viewport is short.
