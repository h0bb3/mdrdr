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

## Sequence diagram — basic

```mermaid
sequenceDiagram
    participant A as Alice
    participant B as Bob
    A->>B: hi there
    B-->>A: hi yourself
    A->>B: still alive?
    B-->>A: yep
```

## Sequence diagram — typical request/response

```mermaid
sequenceDiagram
    participant Op as Operator
    participant S as Service
    participant DB as Database
    Op->>S: POST /thing {payload}
    S->>DB: INSERT row
    DB-->>S: id=42
    S-->>Op: 201 {id: 42}
```

## Sequence diagram — self-call

```mermaid
sequenceDiagram
    participant U as User
    participant API
    participant Cache
    U->>API: GET /widgets
    API->>Cache: lookup
    Cache-->>API: miss
    API->>API: compute (slow path)
    API->>Cache: store
    API-->>U: {widgets: [...]}
```

## Unknown graph falls back to code

A plain code block should still render if the content is not a graph we can parse:

```mermaid
oops not a graph
```
