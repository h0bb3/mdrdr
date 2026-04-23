# Images

Markdown image syntax, loaded from disk and scaled to fit.

## Small logo

A 48×48 PNG rendered at natural size:

![debian logo](logo.png)

The image above sits in a standalone paragraph, which mdrdr treats as a block. The alt text survives in the AST for accessibility later.

## Wider image

A 260×91 PNG also fits within the content width:

![ubuntu wordmark](ubuntu.png)

## Missing image

If the file doesn't exist we fall back to the alt-text placeholder: ![not found](no-such-file.png)

## Inline reference

An inline image mid-sentence still shows as a placeholder, since block layout only kicks in when the paragraph is a single image: here ![tiny](logo.png) sits between words.
