# Mermaid

## A simple flowchart

```mermaid
graph TD
    A[Start] --> B{Decision}
    B --> C[Option A]
    B --> D[Option B]
    C --> E[Done]
    D --> E
```

## Left-to-right with labels

```mermaid
graph LR
    Ingest -->|raw| Parse
    Parse -->|AST| Layout
    Layout -->|frames| Render
    Render -->|pixels| Screen
```

## Shapes

```mermaid
graph TD
    A[rectangle]
    B(rounded)
    C((circle))
    A --> B --> C
```

## Unknown graph falls back to code

A plain code block should still render if the content is not a graph we can parse:

```mermaid
oops not a graph
```
