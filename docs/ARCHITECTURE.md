# Anvil Architecture Contract

This document is the normative architecture contract for Anvil. Implementation details may evolve, but changes must preserve the ownership, identity, and incremental-update rules below. The render-pipeline document describes the execution sequence; this document defines which subsystem owns each kind of truth.

## 1. Product boundary

Anvil is a native UI runtime for the JavaScript, TypeScript, HTML-like component, and CSS ecosystems. It is not a browser embedding layer.

- No Electron, Chromium, WebView, browser process, or browser DOM is part of the runtime.
- JavaScript execution, framework reconciliation, CSS computation, layout, text shaping, and GPU rendering are independent replaceable subsystems.
- Compatibility is behavioral and tested. Having a familiar API or dependency does not imply browser compatibility.
- Native windowing and event delivery are owned by the platform host; GPU resource execution is owned by the renderer.

## 2. Non-negotiable ownership rules

### 2.1 One canonical UI hierarchy

The Deque Tree is the only owner of UI elements and the only authority for logical parent/child and sibling relationships.

- A `UiElement` is stored in exactly one Deque entry and is addressed externally by a stable `ElementHandle`.
- Deque subtree spans and Deque topology encode structural relationships. `UiElement` must not gain a duplicated `children`, parent, or sibling collection.
- `LogicalProjection` is a non-owning read-only view. It derives relationships from the canonical Deque Tree and must not cache a second authoritative hierarchy.
- Framework adapters retain identity mappings only. They must not retain a shadow DOM, virtual tree, widget tree, or duplicate child lists.
- Layout and rendering may retain derived data, but neither may own, move, insert, or delete UI elements.

### 2.2 Deque topology and identity

The current Deque design remains authoritative:

- Initial construction establishes the Deque topology; topology is sealed before normal incremental operation.
- Runtime element mutations operate within existing Deques. Ghost Deques preserve topology when content becomes empty.
- `End::Back` wraps the existing subtree with a new ancestor; `End::Front` inserts a child according to the established span rules.
- Removing a Deque root with descendant branches clears the affected Deques while preserving topology and IDs.
- Moving a Deque root that spans child Deque branches is rejected with `SubtreeSpansMultipleDeques`.
- The registry map value remains exactly `[DequePointer, IdentityHash, DequeId]`; topology links remain in a separate map.
- Identity hashes are local and order-sensitive structural fingerprints. Element identity, order, subtree-span boundaries, and ghost state may affect them. Text, attributes, inline declarations, and computed styles do not.
- Normal mutations use targeted hash checks. Full synchronization is a recovery/fallback path, not the normal render path.

### 2.3 Identity is not geometry

`ElementHandle` identifies the canonical UI element. It does not encode its location, dimensions, style, or paint order. Those values are derived and can change without changing element identity or Deque identity.

## 3. Subsystem authority

| Subsystem | Owns / decides | Must not decide |
| --- | --- | --- |
| Deque Tree / UiModel | Element lifetime, canonical ordering, structural relationships, source attributes and inline declarations | CSS cascade, layout geometry, GPU paint |
| Framework adapter | Framework-ID to Anvil-handle mappings and ordered host commits | A second UI hierarchy or CSS/layout |
| JavaScript engine | JavaScript execution and JS object semantics | UI ownership outside the host commit contract |
| Stylo | CSS parsing, selectors, cascade, inheritance, computed CSS values | Native geometry or paint |
| Text system | Font matching, shaping, glyph fallback, text measurement and raster inputs | Element hierarchy or CSS cascade |
| Layout engine | Formatting constraints, intrinsic sizing, geometry, fragments, layout caches | UI element ownership or a competing source of parent/child truth |
| Renderer | Paint ordering metadata, clipping/compositing, GPU resources and drawing | CSS cascade or layout calculation |
| Window/platform host | Native window, surface lifecycle, input/event delivery | UI tree ownership or rendering policy |

A subsystem may cache its own derived data. It may not silently become an alternative authority for another subsystem's domain.

## 4. Canonical data flow

```text
JavaScript / TypeScript / Framework
                 |
       Framework host adapter
        (stable ID mappings)
                 |
       ordered incremental commits
                 |
       Canonical Deque Tree / UiModel
          /          |           \
         /           |            \
   Stylo CSS    Text / resources   Mutation journal
       |              |                 |
 Computed CSS     intrinsic metrics     |
       \              /                 |
        \            /                  |
          Layout invalidation <----------+
                  |
          Layout computation
                  |
       Derived geometry / fragments
                  |
       Render preparation / paint metadata
                  |
           Native WGPU renderer
                  |
           Native window surface
```

The initial snapshot is consumed once to establish the canonical model. Subsequent changes use ordered incremental commits and targeted invalidation. A stylesheet epoch, viewport change, or device-scale change is an explicit invalidation source; it is not disguised as a structural mutation.

## 5. CSS and computed-style contract

Stylo is the single authority for CSS parsing, selector matching, cascade, inheritance, and computed values.

- Inline declarations are source input, not final computed style.
- Layout consumes computed values emitted by Stylo. It does not parse stylesheet text, recascade declarations, or independently resolve inheritance.
- Computed values must be represented in a form that preserves the information required by consumers. Opaque or unresolved CSS values must not be silently converted to unrelated defaults.
- Changes to computed CSS are a distinct derived-state change. They must be observable by Layout and Renderer without being fed back as user mutations that cause Stylo to recascade its own output.
- Consumers must be able to track their own revision/cursor. One consumer acknowledging a change must not erase it before another consumer has observed it.
- Consumers should compare old and new computed values and classify changed properties. Geometry-affecting properties invalidate layout; paint-only properties invalidate render preparation; inherited changes may affect descendants.

The current serialized `BTreeMap<String, String>` is an interchange boundary, not a commitment that every CSS value must forever be flattened to text. Typed computed values or retained Stylo value handles may be introduced where serialization loses required semantics, with explicit lifetime ownership.

### 5.1 Computed-style delta stream

Computed-style output has its own monotonic revision and journal, separate from the source UI mutation journal.

- Replacing an element's computed map compares old and new property values and publishes a delta only when at least one property was actually added, removed, or changed.
- Each delta records the stable `ElementHandle`, its revision, and the sorted set of changed property names. The latest computed values remain in `UiModel`; consumers read values there rather than carrying duplicate full maps.
- Computed-style publication does not increment the source UI mutation revision and is never fed back into Stylo's input mutation stream.
- Layout and Renderer have independent cursors. Acknowledging one consumer cannot discard records that the other has not consumed.
- Journal entries are reclaimed only up to the minimum acknowledged revision across both consumers.
- A consumer may coalesce multiple deltas for the same element and inspect its latest computed values. It must classify geometry-affecting and paint-only properties according to its own responsibility.
- Consumers initialize from a complete current snapshot, then advance from their cursor. A newly initialized consumer may set its cursor to the current revision after taking that snapshot.

## 6. Layout is a solver, not another UI tree

Layout is required because the UI hierarchy and computed CSS describe inputs and constraints, not final geometry. The Layout subsystem calculates the result; it does not own another copy of the UI.

### 6.1 Inputs

The solver consumes:

1. The canonical element order and relationships through `LogicalProjection`.
2. Layout-affecting computed CSS values from Stylo.
3. Intrinsic measurements from the text system and other replaced/content resources.
4. Explicit viewport, available-space, writing-mode, and device-scale inputs.
5. Targeted structural, content, and computed-style invalidations.

### 6.2 Outputs

Layout produces derived output:

- Border/content box geometry and the positions required by downstream painting.
- Text line and glyph-run placement, including multiple fragments where content wraps.
- Fragment/box metadata needed for clipping, hit testing, and paint preparation.
- Per-element lookup by stable `ElementHandle` where a one-element-to-one-result mapping is valid.
- A list of fragments for an element when it generates multiple visual fragments.

A single rectangle per element is a useful initial milestone, not a universal CSS model. Inline content, fragmentation, pseudo/generated content, and anonymous boxes can create zero, one, or multiple layout objects for an element.

### 6.3 Derived formatting objects

Some CSS formatting behavior cannot be represented by a one-to-one mapping from UI elements to layout nodes. Examples include anonymous boxes, `display: contents`, inline formatting, and fragmented content.

When an algorithm needs such objects, Layout may generate a derived formatting/fragment projection. This is not a second UI tree:

- It contains layout-only objects, not cloned `UiElement` values.
- Each object references its originating `ElementHandle` when one exists; anonymous objects may have no direct element owner.
- Its relationships are derived from current canonical structure and computed CSS.
- It cannot mutate canonical Deque topology or become the source of framework-visible parent/child relationships.
- It is disposable/reconcilable derived state and is invalidated from explicit revisions.

For simple formatting contexts, the solver should traverse the canonical projection directly. Derived formatting objects are introduced only where CSS semantics require them.

### 6.4 Backend boundary

Taffy may provide algorithms for supported Block, Flexbox, Grid, and other formatting modes. It is an implementation backend, not Anvil's architecture or source of truth.

- Use low-level integration over Anvil's canonical/derived projection; do not create a long-lived high-level TaffyTree that duplicates all UI nodes and hierarchy.
- Any Taffy node IDs are backend identifiers mapped to stable Anvil identities or derived layout-object IDs.
- Backend caches and style records are derived state.
- Backend limitations must be surfaced as explicit unsupported behavior and tests; enabling a Cargo feature is not proof that Anvil supports that CSS behavior.
- The backend can be replaced without changing Deque Tree, FrameworkCommit, Stylo authority, or renderer contracts.

## 7. Incremental layout and invalidation

The initial layout may require an initial walk to establish state. Normal updates must be proportional to affected elements and formatting contexts, not total UI size.

Invalidation sources are distinct:

| Change | Minimum affected scope |
| --- | --- |
| Text content | Text shaping/measurement, its fragments, and ancestor formatting contexts whose intrinsic size depends on it |
| Attribute or inline-style source change | Stylo selector/style invalidation; after computed values change, invalidate the relevant layout or paint consumers |
| Computed geometry property | Element's layout state, dependent descendants, and ancestor contexts affected by intrinsic/percentage constraints |
| Computed paint-only property | Render preparation/paint state; do not rerun layout |
| Insert/remove | Affected subtree and old/new parent formatting contexts |
| Move | Moved subtree and both old/new parent formatting contexts |
| Font/resource load or font metric change | Text measurements and dependent ancestor layout contexts |
| Viewport / available-space change | Layout contexts dependent on those constraints |
| Device-scale change | Pixel rounding and raster/render resources; CSS-pixel geometry remains separately represented |
| Stylesheet epoch change | Stylo's bounded recalculation schedule, followed by consumer-specific style invalidation |

The mutation journal must carry enough stable identity and revision information for consumers to derive affected contexts without storing cloned sibling lists. Layout and renderer consume computed-style deltas through their own cursors or equivalent independent revision tracking.

A cache is reusable only if every input that affects its result is unchanged. Cache validity must include relevant computed-style version, available space, intrinsic measurement version, and formatting-context dependencies.

Do not perform a full UI traversal merely to render a frame. Dirty layout work may inspect the affected formatting context and the ancestors/descendants required by its dependencies. Stylesheet-wide work may be processed in bounded batches, but it must not be described as constant-time.

## 8. Rendering contract

The Renderer consumes prepared render data. It does not ask Stylo to compute CSS or Layout to solve geometry during GPU drawing.

The prepared data may include:

- Stable `ElementHandle` and references to canonical element data.
- Computed visual values needed for drawing.
- Final CSS-pixel geometry and device-pixel-rounded geometry as separate values.
- Text/glyph runs or resource references from the text system.
- Clipping, transforms, opacity, stacking-context, and compositing metadata.
- Paint-order information generated from CSS formatting/stacking rules.

The Deque Tree's parent-first preorder is canonical structural traversal order. It is not automatically the final CSS paint order for every stacking context, positioned element, transform, or compositing case. Paint preparation must derive the required ordering metadata without changing canonical UI ownership.

The GPU renderer receives prepared batches/commands and handles GPU resources, drawing, and surface presentation. It must not perform CSS parsing, selector matching, or layout solving.

## 9. Framework and JavaScript boundary

All framework adapters converge on one framework-neutral contract:

1. One initial snapshot with stable framework IDs, ordered Deque groups, and validated subtree spans.
2. Ordered incremental operations for creation, text, properties, inline styles, removal, and movement.
3. Stable ID mappings to Anvil `ElementHandle` and Deque handles.
4. No shadow tree retained by the adapter.

The target ecosystem includes React, Preact, Vue, Svelte, Solid, Angular, and Vanilla JavaScript/TypeScript. They are not declared supported merely because the generic adapter exists. Each needs a real adapter and behavior tests.

The JavaScript engine is replaceable. Framework-facing bindings should expose the commit contract rather than expose internal Deque pointers, Taffy IDs, Stylo objects, or GPU implementation details.

## 10. Required development and verification cycle

Every feature follows this exact loop:

1. **Design:** define the contract, ownership, affected modules, complexity expectations, and failure behavior.
2. **Development:** implement the smallest coherent vertical slice on `main`.
3. **Verification:** inspect the resulting diff and confirm invariants and public API boundaries.
4. **Testing:** add focused unit/integration tests and run the complete CI suite.
5. **Validation and stabilization:** inspect actual CI job results, test regressions and architecture drift; fix failures and repeat until green.
6. **Continue:** only after stabilization begin the next feature.

A green CI run is necessary but not sufficient for architectural validation. A feature is complete only when code, tests, and ownership contracts agree.

## 11. Reconstruction sequence

The current implementation is an experimental baseline, not a reason to preserve incorrect boundaries. Rebuild in small, independently verifiable stages:

1. **[Implemented foundation] Change propagation contract:** distinguish source UI mutations from computed-style deltas; give Layout and Renderer independent invalidation cursors.
2. **Persistent derived layout state:** keep stable identity mappings and layout caches across passes; remove repeated whole-projection setup from ordinary frames.
3. **Geometry-to-render contract:** pass final geometry/fragments and computed visual state to render preparation, while keeping GPU drawing independent.
4. **Formatting coverage:** add correct block/inline, flex, grid, positioning, intrinsic sizing, and fragment behavior as separately tested milestones.
5. **Paint preparation:** derive stacking, clipping, transforms, hit testing, and compositing metadata from computed style and layout output.
6. **Framework adapters and bindings:** build real framework host adapters only after the canonical commit and render pipeline contracts are stable.

The order may change when code inspection demonstrates a dependency, but no stage may introduce a second authoritative UI hierarchy or bypass the design/development/verification/testing/stabilization loop.

## 12. Known gaps in the current baseline

The following are implementation gaps observed in the current source and must be closed during reconstruction:

- The computed-style journal and independent Layout/Renderer cursors are implemented, but the concrete Layout and Renderer consumers are not yet wired to acknowledge and process their deltas.
- `NativeLayoutPass` currently builds per-pass state and a full `ElementHandle` projection; its Taffy IDs and measurement caches are not persistent across passes.
- Current layout computation accepts flex containers with children and text/leaf nodes, but rejects non-flex containers with children. The existence of Block/Grid Taffy features does not make these modes implemented.
- The current `RenderInput` carries elements but no layout geometry/fragments or complete prepared visual state.
- The Winit redraw callback is currently a placeholder; the native window, layout, renderer, and surface lifecycle are not yet a complete presentation loop.

These are explicit reconstruction targets, not hidden assumptions.
