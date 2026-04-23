# Tables

## Basic

| Name     | Role      | Dept        |
| -------- | --------- | ----------- |
| Ada      | Engineer  | Platform    |
| Linus    | Dev       | Kernel      |
| Grace    | Admiral   | Navy        |

## Alignment

| Left       | Centered      |         Right |
| :--------- | :-----------: | ------------: |
| aligned    |   center      |          123  |
| left       |   mid         |         4,567 |
| naturally  |   so          |        98,765 |

## Inline formatting in cells

| Command        | Description                            | Docs     |
| -------------- | -------------------------------------- | -------- |
| `mdrdr render` | Write a **PNG** and *exit*             | [readme](README.md) |
| `mdrdr open`   | Launch a native window with the ***API*** live | --- |

## Long cells wrap

| Topic        | Explanation                                                                 |
| ------------ | --------------------------------------------------------------------------- |
| word wrap    | Each cell is a miniature paragraph — words break at whitespace and the cell grows vertically to accommodate them, so you get natural line breaks rather than horizontal overflow. |
| alignment    | Controlled by colons in the separator row. `:---:` is center, `---:` is right, and plain `:---` or `---` is left (the default). |
