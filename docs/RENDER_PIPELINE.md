# Native Render Pipeline

## Purpose

Anvil is a native rendering runtime for the web ecosystem. It must preserve the web-facing authoring model while avoiding Chromium, Electron, WebView, and a browser DOM runtime.

The goal is not to reproduce a browser process architecture. JavaScript, TypeScript, framework reconciliation, HTML-like elements, and CSS are inputs to Anvil's native runtime. They must not require a second browser-owned UI hierarchy.

## Non-negotiable invariants

1. The Deque Tree remains the only owner of UI elements and the canonical parent/child relationships.
2. Framework integrations translate framework host operations into Anvil commits. They do not maintain a parallel DOM, virtual DOM, or native widget tree.
3. CSS is computed by Stylo against the non-owning LogicalProjection.
4. Layout data is derived state keyed by stable ElementHandle values. It must not become another owner of elements or another source of parent/child truth.
5. Rendering consumes geometry and computed visual state from the native pipeline. It does not ask a browser engine to paint.
6. The JavaScript engine, CSS engine, layout engine, text system, and GPU renderer remain replaceable boundaries.

## Pipeline

```text
JavaScript / TypeScript / Framework
                 |
         Framework host adapter
                 |
        ordered incremental commit
                 |
       Canonical Deque Tree (UiModel)
                 |
       +---------+----------+
       |                    |
   Stylo CSS             mutation journal
       |                    |
 computed CSS values   targeted invalidation
       |                    |
       +---------+----------+
                 |
          Native layout
                 |
       text measurement / shaping
                 |
        layout rectangles
                 |
        WGPU native renderer
                 |
          Winit surface
```

The framework adapter is not a rendering engine. It translates a framework's host operations into the same framework-neutral commit contract. React, Preact, Vue, Svelte, Solid, Angular, and Vanilla JS/TS must all converge on that contract.

## Layout backend direction

Taffy is a candidate backend for the initial CSS box-layout algorithms because it provides Block, Flexbox, and Grid algorithms and exposes a low-level API intended for integration with an existing tree.

Anvil must use the low-level integration path, not create a long-lived high-level TaffyTree that duplicates the UI hierarchy. The adapter should implement Taffy's traversal and layout traits over the canonical LogicalProjection:

- Child enumeration comes from LogicalProjection::children_of.
- Stable Anvil element handles are mapped to Taffy node identifiers through an index only. The mapping owns no elements and stores no parent/child lists.
- Per-element layout state may cache converted layout styles, Taffy measurement caches, and computed rectangles. It must not own UI nodes or structural relationships.
- Taffy layout algorithms operate on the canonical tree's current child relationships.
- Text and replaced-element measurement are delegated to Anvil's text and resource systems.

Taffy is not a complete browser layout engine. Its documented algorithms cover Block, Flexbox, and Grid, with float support behind its feature. Inline formatting, full table layout, fragmentation, and other browser layout behavior must be tracked as explicit compatibility work. Anvil must not claim full CSS layout support merely because Stylo and Taffy are present.

## CSS-to-layout boundary

Stylo remains the authority for CSS parsing, cascade, selector matching, inheritance, and computed values. The layout adapter must consume computed values, not independently recascade stylesheet text or treat inline declarations as the final style.

The current serialized computed-style surface is intentionally small. Before claiming useful web layout compatibility, it must be expanded to include the layout-affecting computed values required by the chosen algorithms, including at least:

- display and box sizing
- width, height, min/max sizes and aspect ratio
- margin, padding, and border widths
- position and inset values
- flex direction, wrap, basis, grow, shrink, alignment, and gaps
- grid templates, auto tracks, placement, and gaps
- direction and writing-mode inputs where supported

Unsupported or unrepresentable computed values must be reported or deliberately handled. They must not silently become arbitrary defaults.

## Invalidation and caching

Layout caches are keyed by stable element identity and invalidated from the existing mutation and style revisions.

- Text changes invalidate the measured text node and the ancestor layout contexts whose intrinsic sizes can change.
- Attribute and inline-style changes invalidate the affected style state and any dependent layout state.
- Insertions, removals, and moves invalidate the old and new parent layout contexts, the affected subtrees, and any formatting context whose child ordering changed.
- Viewport or device-scale changes invalidate layout inputs without rebuilding the canonical UI hierarchy.
- A stylesheet epoch change invalidates computed style and layout work according to the stylesheet recalculation schedule.

Correctness comes before batching. A cache may be reused only when its full set of layout inputs is unchanged.

## Renderer contract

The renderer must receive stable element identity, the canonical element data, computed visual properties, and a final layout rectangle. Layout geometry is output state, not a new structural tree.

The render traversal remains the canonical Deque Tree's parent-first order. Paint ordering, stacking contexts, clipping, transforms, hit testing, and compositing must be layered on explicit render metadata rather than by changing canonical ownership.

## Compatibility policy

Anvil aims to support the web ecosystem without requiring the browser platform implementation. Compatibility is therefore measured by concrete behavior:

- framework host operations and lifecycle behavior
- HTML element and attribute semantics needed by those frameworks
- CSS parsing, cascade, selectors, and computed values
- layout algorithms and text metrics
- event dispatch, focus, input, accessibility, and resource loading

A feature is supported only when its behavior is implemented and tested. A familiar API name alone is not compatibility.

## Immediate implementation sequence

1. Establish the native layout boundary and its data contract.
2. Implement the Taffy low-level adapter over LogicalProjection, without a second UI tree.
3. Expand Stylo's computed-style export for layout inputs.
4. Connect text measurement and final geometry to the renderer.
5. Add differential and conformance tests for layout and invalidation.
6. Build real framework host adapters against the stable commit contract.
7. Add the public JavaScript/TypeScript bindings that expose the runtime to applications and adapters.

The sequence is intentionally renderer-first: framework integrations should not be built on top of a runtime that cannot yet calculate and deliver native geometry.
